//! Bounded ancestry checks against an observed head's execution context.
//! These observations authorize no cached state read or money action.

use crate::{Client, client::HeadObservation};
use alloy::eips::BlockId;
use alloy::network::TransactionBuilder;
use alloy::primitives::{Address, B256, Bytes, U256};
use alloy::providers::Provider;
use alloy::rpc::types::{TransactionRequest, state::AccountOverride};
use alloy_sol_types::SolCall;
use anyhow::{Context, Result, bail};

alloy_sol_types::sol!("../../contracts/evm/src/HeaderRead.sol");

/// The hash of an earlier block in this head's branch. A node executing at
/// another height or parent cannot substitute its numbered canonical view.
/// Sibling heads sharing this parent have the same ancestry below it.
/// Unsupported or ambiguous answers leave the ordinary header walk intact.
pub async fn ancestor(client: &Client, head: &HeadObservation, number: u64) -> Result<B256> {
    if number >= head.number || head.number - number > 256 {
        bail!("the ancestry query is outside the EVM blockhash window");
    }
    let endpoint = client
        .each_endpoint()
        .into_iter()
        .next()
        .context("no ancestry endpoint")?;
    let runtime = Bytes::from(alloy::hex::decode(
        include_str!("sealed_views/HeaderRead.hex").trim(),
    )?);
    let call = HeaderRead::readCall {
        ancestorNumber: U256::from(number),
    };
    let request = TransactionRequest::default()
        .with_from(Address::ZERO)
        .with_to(Address::ZERO)
        .with_gas_limit(client.rpc_policy().view_inner_gas)
        .with_input(call.abi_encode());
    let returned = endpoint
        .call(request)
        .block(BlockId::hash_canonical(head.hash))
        .account_override(Address::ZERO, AccountOverride::default().with_code(runtime))
        .await?;
    if returned.len() != 128 {
        bail!("the ancestry observation has another output length");
    }
    let read = HeaderRead::readCall::abi_decode_returns(&returned)?;
    if read.number != U256::from(head.number)
        || read.timestamp != U256::from(head.timestamp)
        || read.parentHash != head.parent_hash
        || read.ancestorHash == B256::ZERO
    {
        bail!("the ancestry observation does not match the selected head");
    }
    Ok(read.ancestorHash)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::keccak256;
    #[test]
    fn the_ancestry_runtime_matches_its_recorded_hash() {
        let bytes = alloy::hex::decode(include_str!("sealed_views/HeaderRead.hex").trim()).unwrap();
        let expected: B256 = include_str!("sealed_views/HeaderRead.hash")
            .trim()
            .parse()
            .unwrap();
        assert_eq!(keccak256(bytes), expected);
        let source = include_bytes!("../../../contracts/evm/src/HeaderRead.sol");
        let source_hash: B256 = include_str!("sealed_views/HeaderRead.source-hash")
            .trim()
            .parse()
            .unwrap();
        assert_eq!(
            keccak256(source),
            source_hash,
            "rebuild the ancestry runtime after changing its source"
        );
    }
    async fn node(answer: serde_json::Value) -> Client {
        use serde_json::json;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let answer = answer.clone();
                tokio::spawn(async move {
                    let mut bytes = Vec::new();
                    let (start, size) = loop {
                        let mut part = [0; 1024];
                        let read = stream.read(&mut part).await.unwrap();
                        if read == 0 {
                            return;
                        }
                        bytes.extend_from_slice(&part[..read]);
                        assert!(bytes.len() < 65536);
                        if let Some(end) = bytes.windows(4).position(|p| p == b"\r\n\r\n") {
                            let header = std::str::from_utf8(&bytes[..end]).unwrap();
                            let size: usize = header
                                .lines()
                                .find_map(|line| {
                                    let (key, value) = line.split_once(':')?;
                                    key.eq_ignore_ascii_case("content-length")
                                        .then(|| value.trim().parse().unwrap())
                                })
                                .unwrap();
                            if bytes.len() >= end + 4 + size {
                                break (end + 4, size);
                            }
                        }
                    };
                    let request: serde_json::Value =
                        serde_json::from_slice(&bytes[start..start + size]).unwrap();
                    let result = match request["method"].as_str().unwrap_or_default() {
                        "eth_chainId" => json!("0x7a69"),
                        "eth_call" => answer,
                        _ => serde_json::Value::Null,
                    };
                    let body = serde_json::to_vec(
                        &json!({"jsonrpc":"2.0","id":request["id"],"result":result}),
                    )
                    .unwrap();
                    let header = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    stream.write_all(header.as_bytes()).await.unwrap();
                    stream.write_all(&body).await.unwrap();
                });
            }
        });
        let config = crate::ChainConfig {
            label: "ancestry".into(),
            rpc: url.clone(),
            public_rpc: url,
            ws: None,
            dollar: "USDC".into(),
            native_usd: None,
            logs_rpc: None,
            log_range: None,
            confirmations: None,
        };
        Client::connect(&config, crate::Signing::ReadOnly)
            .await
            .unwrap()
    }

    fn head() -> HeadObservation {
        HeadObservation {
            number: 20,
            timestamp: 120,
            hash: B256::repeat_byte(20),
            parent_hash: B256::repeat_byte(19),
            logs_bloom: Default::default(),
        }
    }

    fn output(number: u64, timestamp: u64, parent: B256, ancestor: B256) -> serde_json::Value {
        let result = HeaderRead::readReturn {
            number: U256::from(number),
            timestamp: U256::from(timestamp),
            parentHash: parent,
            ancestorHash: ancestor,
        };
        serde_json::json!(format!(
            "0x{}",
            alloy::hex::encode(HeaderRead::readCall::abi_encode_returns(&result))
        ))
    }

    #[tokio::test]
    async fn another_nodes_head_cannot_supply_the_ancestry() {
        let selected = head();
        for (number, time, parent) in [
            (19, 119, B256::repeat_byte(18)),
            (20, 119, selected.parent_hash),
            (20, 120, B256::repeat_byte(80)),
        ] {
            let client = node(output(number, time, parent, B256::repeat_byte(10))).await;
            assert!(ancestor(&client, &selected, 10).await.is_err());
        }
    }

    #[tokio::test]
    async fn a_valid_changed_ancestor_is_reported_instead_of_hidden() {
        let selected = head();
        let changed = B256::repeat_byte(80);
        let client = node(output(
            selected.number,
            selected.timestamp,
            selected.parent_hash,
            changed,
        ))
        .await;
        assert_eq!(ancestor(&client, &selected, 10).await.unwrap(), changed);
        assert_ne!(changed, B256::repeat_byte(10));
    }

    #[tokio::test]
    async fn absent_oversized_and_out_of_range_ancestry_stays_unproved() {
        let selected = head();
        let oversized = "0x".to_string() + &"00".repeat(160);
        for answer in [
            serde_json::json!("0x"),
            serde_json::json!(oversized),
            output(
                selected.number,
                selected.timestamp,
                selected.parent_hash,
                B256::ZERO,
            ),
        ] {
            let client = node(answer).await;
            assert!(ancestor(&client, &selected, 10).await.is_err());
        }
        let client = node(output(
            selected.number,
            selected.timestamp,
            selected.parent_hash,
            B256::repeat_byte(10),
        ))
        .await;
        assert!(ancestor(&client, &selected, 20).await.is_err());
        let mut far = selected;
        far.number = 300;
        assert!(ancestor(&client, &far, 10).await.is_err());
    }
    #[tokio::test]
    async fn the_ancestry_runtime_executes_in_a_real_blocks_context() {
        use std::process::{Child, Command, Stdio};
        struct OwnedNode(Child);
        impl Drop for OwnedNode {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let reserved = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = reserved.local_addr().unwrap();
        let port = address.port().to_string();
        drop(reserved);
        let _node = OwnedNode(
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
            label: "ancestry runtime".into(),
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
        let _: serde_json::Value = client
            .provider()
            .raw_request("anvil_mine".into(), ["0x8"])
            .await
            .unwrap();
        let head = client.head_details().await.unwrap();
        assert_eq!(head.number, 8);
        assert_eq!(
            ancestor(&client, &head, 3).await.unwrap(),
            client.block_identity(3).await.unwrap()
        );
        assert_eq!(ancestor(&client, &head, 7).await.unwrap(), head.parent_hash);
    }
}
