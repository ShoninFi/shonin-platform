//! Sparse header ancestry retained only while connected to the selected head.

use crate::{Client, client::HeadObservation};
use anyhow::{Context, Result, bail};
use std::collections::{BTreeMap, VecDeque};

#[derive(Default)]
pub struct BranchProofs {
    head: Option<HeadObservation>,
    known: BTreeMap<u64, HeadObservation>,
    order: VecDeque<u64>,
    remaining: u64,
    capacity: usize,
    compact_available: bool,
    pending: Option<Box<BranchProofs>>,
}

impl BranchProofs {
    pub fn head(&self) -> Option<&HeadObservation> {
        self.head.as_ref()
    }

    fn starting(head: HeadObservation, client: &Client) -> Self {
        let mut proofs = Self {
            remaining: client.rpc_policy().ancestry_reads,
            capacity: client.rpc_policy().ancestry_cache_headers as usize,
            compact_available: true,
            head: Some(head.clone()),
            ..Self::default()
        };
        proofs.remember(head);
        proofs
    }

    /// True means the old derived record must be rebuilt. Unsupported ancestry
    /// leaves a dated record; it never authorizes keeping an unconnected prefix.
    /// A completed pending candidate can precede `next`: callers must use
    /// `head()` as the read scope and preserve staleness until they reach `next`.
    pub async fn select(&mut self, client: &Client, next: HeadObservation) -> Result<bool> {
        let Some(previous) = self.head.clone() else {
            *self = Self::starting(next, client);
            return Ok(false);
        };
        if next.hash == previous.hash {
            Self::same_header(&next, &previous)?;
            self.remaining = client.rpc_policy().ancestry_reads;
            self.compact_available = true;
            self.pending = None;
            return Ok(false);
        }
        let mut fresh = Self::starting(next.clone(), client);
        if next.number <= previous.number {
            *self = fresh;
            return Ok(true);
        }
        if let Some(pending) = self.pending.take() {
            let candidate_head = pending.head.as_ref().unwrap();
            if next.hash == candidate_head.hash {
                Self::same_header(&next, candidate_head)?;
                fresh = *pending;
            } else if next.number > candidate_head.number {
                // Finish this fixed candidate before chasing advancing tips.
                // Its canonicality is checked by the caller before publication;
                // a lagging candidate must not renew record freshness.
                fresh = *pending;
            }
            fresh.remaining = client.rpc_policy().ancestry_reads;
            fresh.compact_available = true;
        }
        let ancestor = match fresh.at(client, previous.number).await {
            Ok(header) => header,
            Err(error) => {
                self.pending = Some(Box::new(fresh));
                return Err(error);
            }
        };
        let replaced = ancestor.hash != previous.hash;
        if !replaced {
            fresh.merge(self)?;
        }
        // The head-comparison legs must not consume historical catch-up work.
        fresh.remaining = client.rpc_policy().ancestry_reads;
        fresh.compact_available = true;
        fresh.pending = None;
        *self = fresh;
        Ok(replaced)
    }

    fn same_header(left: &HeadObservation, right: &HeadObservation) -> Result<()> {
        if left.number != right.number
            || left.hash != right.hash
            || left.timestamp != right.timestamp
            || left.parent_hash != right.parent_hash
            || left.logs_bloom != right.logs_bloom
        {
            bail!("the same selected header has contradictory details");
        }
        Ok(())
    }

    fn merge(&mut self, previous: &Self) -> Result<()> {
        for number in &previous.order {
            if let Some(header) = previous.known.get(number) {
                if let Some(current) = self.known.get(number) {
                    Self::same_header(current, header)?;
                }
                self.remember(header.clone());
            }
        }
        Ok(())
    }

    fn spend(&mut self) -> Result<()> {
        if self.remaining == 0 {
            bail!("selected-branch ancestry exhausted its configured read budget");
        }
        self.remaining -= 1;
        Ok(())
    }

    fn remember(&mut self, header: HeadObservation) {
        let number = header.number;
        if self.known.insert(number, header).is_none() {
            self.order.push_back(number);
        }
        while self.known.len() > self.capacity {
            if let Some(number) = self.order.pop_front() {
                self.known.remove(&number);
            }
        }
    }

    /// Establish a header by following hashes from a known descendant. Sparse
    /// BLOCKHASH checkpoints accelerate long history; unsupported helpers fall
    /// back to exact parent-hash requests. Progress survives a budget refusal.
    pub async fn at(&mut self, client: &Client, number: u64) -> Result<HeadObservation> {
        let head = self.head.as_ref().context("no selected branch head")?;
        if number > head.number {
            bail!("a header is newer than the selected branch head");
        }
        if let Some(header) = self.known.get(&number) {
            return Ok(header.clone());
        }
        let mut current = self
            .known
            .range(number..)
            .next()
            .map(|(_, h)| h.clone())
            .unwrap_or_else(|| head.clone());
        while current.number > number {
            let distance = current.number - number;
            let (target, hash) = if distance > 1 && self.compact_available && self.remaining >= 2 {
                let target = number.max(current.number.saturating_sub(256));
                self.spend()?;
                match crate::header_read::ancestor(client, &current, target).await {
                    Ok(hash) => (target, hash),
                    Err(_) => {
                        self.compact_available = false;
                        (current.number - 1, current.parent_hash)
                    }
                }
            } else {
                (current.number - 1, current.parent_hash)
            };
            self.spend()?;
            let earlier = client.header_by_hash(hash).await?;
            if earlier.number != target || earlier.timestamp > current.timestamp {
                bail!("an ancestor header does not match its selected descendant");
            }
            self.remember(earlier.clone());
            current = earlier;
        }
        Ok(current)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::Address;
    use alloy::providers::Provider;
    use alloy::rpc::types::Filter;
    use std::process::{Child, Command, Stdio};
    use std::sync::Arc;
    struct OwnedNode(Child);
    impl Drop for OwnedNode {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    async fn node() -> (OwnedNode, Client) {
        let reserved = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = reserved.local_addr().unwrap();
        let port = address.port().to_string();
        drop(reserved);
        let node = OwnedNode(
            Command::new("anvil")
                .args(["--silent", "--port", &port, "--chain-id", "31337"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("anvil is required for the ancestry runtime test"),
        );
        let stop = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while tokio::net::TcpStream::connect(address).await.is_err() {
            assert!(
                std::time::Instant::now() < stop,
                "the owned test node did not start"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let url = format!("http://{address}");
        let config = crate::ChainConfig {
            label: "branch ancestry".into(),
            rpc: url.clone(),
            public_rpc: url,
            ws: None,
            dollar: "USDC".into(),
            native_usd: None,
            logs_rpc: None,
            log_range: None,
            confirmations: None,
        };
        let client = Client::connect(&config, crate::Signing::ReadOnly)
            .await
            .unwrap();
        (node, client)
    }
    async fn mine(client: &Client, blocks: u64) {
        let _: serde_json::Value = client
            .provider()
            .raw_request("anvil_mine".into(), [format!("0x{blocks:x}")])
            .await
            .unwrap();
    }
    #[tokio::test]
    async fn a_forward_fork_discards_earlier_hash_proofs() {
        let (_node, client) = node().await;
        mine(&client, 8).await;
        let snapshot: serde_json::Value = client
            .provider()
            .raw_request("evm_snapshot".into(), ())
            .await
            .unwrap();
        mine(&client, 2).await;
        let old = client.head_details().await.unwrap();
        let mut proofs = BranchProofs::default();
        assert!(!proofs.select(&client, old.clone()).await.unwrap());
        let earlier = proofs.at(&client, 9).await.unwrap();
        let reverted: bool = client
            .provider()
            .raw_request("evm_revert".into(), [snapshot])
            .await
            .unwrap();
        assert!(reverted);
        let _: serde_json::Value = client
            .provider()
            .raw_request("evm_setNextBlockTimestamp".into(), [old.timestamp + 100])
            .await
            .unwrap();
        mine(&client, 4).await;
        let new = client.head_details().await.unwrap();
        assert!(new.number > old.number);
        assert!(proofs.select(&client, new).await.unwrap());
        let replacement = proofs.at(&client, 9).await.unwrap();
        assert_ne!(replacement.hash, earlier.hash);
        assert_eq!(replacement.hash, client.block_identity(9).await.unwrap());
    }
    #[tokio::test]
    async fn minimum_ancestry_work_keeps_progress_and_refusals_stay_dated() {
        let (_node, client) = node().await;
        mine(&client, 16).await;
        let policy = crate::rpc_policy::RpcPolicy {
            ancestry_reads: 1,
            ancestry_cache_headers: 5,
            log_batch_blocks: 1,
            ..Default::default()
        };
        policy.validate("evm:31337").unwrap();
        let client = client.with_rpc_policy(policy);
        let head = client.head_details().await.unwrap();
        let mut proofs = BranchProofs::default();
        proofs.select(&client, head.clone()).await.unwrap();
        let branch = Arc::new(tokio::sync::Mutex::new(proofs));
        let watched = Address::repeat_byte(7);
        let refused = client.verifying_branch(&head, branch.clone(), vec![watched]);
        assert!(
            refused
                .logs(&Filter::new().address(watched).from_block(0).to_block(0))
                .await
                .is_err()
        );
        assert!(refused.ensure_record_branch().is_err());
        let mut complete = false;
        for _ in 0..32 {
            let mut proofs = branch.lock().await;
            assert!(!proofs.select(&client, head.clone()).await.unwrap());
            if proofs.at(&client, 0).await.is_ok() {
                complete = true;
                break;
            }
            assert!(proofs.known.len() <= 5);
        }
        assert!(complete, "a refusal must retain useful historical progress");
        let verified = client.verifying_branch(&head, branch, vec![watched]);
        assert!(
            verified
                .logs(&Filter::new().address(watched).from_block(0).to_block(0))
                .await
                .unwrap()
                .is_empty()
        );
        verified.ensure_record_branch().unwrap();
        assert!(
            refused.ensure_record_branch().is_err(),
            "an earlier refused observation remains dated"
        );
    }
    #[tokio::test]
    async fn moving_heads_do_not_erase_a_bounded_ancestry_catch_up() {
        let (_node, client) = node().await;
        mine(&client, 2).await;
        let policy = crate::rpc_policy::RpcPolicy {
            ancestry_reads: 2,
            ..Default::default()
        };
        let client = client.with_rpc_policy(policy);
        let original = client.head_details().await.unwrap();
        let mut proofs = BranchProofs::default();
        proofs.select(&client, original.clone()).await.unwrap();
        mine(&client, 512).await;
        let candidate = client.head_details().await.unwrap();
        assert!(proofs.select(&client, candidate).await.is_err());
        assert_eq!(
            proofs.head().unwrap().hash,
            original.hash,
            "an unfinished proof cannot adopt the candidate"
        );
        let mut caught_up = false;
        for _ in 0..8 {
            mine(&client, 1).await;
            let selected = client.head_details().await.unwrap();
            match proofs.select(&client, selected.clone()).await {
                Ok(replaced) => {
                    assert!(!replaced);
                    assert!(proofs.head().unwrap().number <= selected.number);
                    assert_eq!(
                        proofs.at(&client, original.number).await.unwrap().hash,
                        original.hash
                    );
                    caught_up = true;
                    break;
                }
                Err(_) => assert_eq!(proofs.head().unwrap().hash, original.hash),
            }
        }
        assert!(
            caught_up,
            "joining a moving tip must retain the older candidate frontier"
        );
    }
    #[tokio::test]
    async fn minimum_work_finishes_a_fixed_candidate_while_tips_advance_faster() {
        let (_node, client) = node().await;
        mine(&client, 2).await;
        let client = client.with_rpc_policy(crate::rpc_policy::RpcPolicy {
            ancestry_reads: 1,
            log_batch_blocks: 1,
            ancestry_cache_headers: 5,
            ..Default::default()
        });
        let original = client.head_details().await.unwrap();
        let mut proofs = BranchProofs::default();
        proofs.select(&client, original.clone()).await.unwrap();
        mine(&client, 16).await;
        let candidate = client.head_details().await.unwrap();
        assert!(proofs.select(&client, candidate.clone()).await.is_err());
        let mut completed = false;
        for _ in 0..20 {
            mine(&client, 2).await;
            let latest = client.head_details().await.unwrap();
            match proofs.select(&client, latest.clone()).await {
                Ok(replaced) => {
                    assert!(!replaced);
                    assert_eq!(proofs.head().unwrap().hash, candidate.hash);
                    assert!(candidate.number < latest.number);
                    assert_eq!(
                        client.block_identity(candidate.number).await.unwrap(),
                        candidate.hash
                    );
                    completed = true;
                    break;
                }
                Err(_) => assert_eq!(proofs.head().unwrap().hash, original.hash),
            }
            assert!(proofs.pending.as_ref().unwrap().known.len() <= 5);
        }
        assert!(
            completed,
            "fixed-candidate work must survive faster moving tips"
        );
    }
}
