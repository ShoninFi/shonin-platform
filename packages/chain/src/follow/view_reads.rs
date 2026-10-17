//! The ViewReads helper: code put at the zero address by a state override
//! for one `eth_call`, never deployed. It runs a bounded list of reads, each
//! as a staticcall with its own gas and result caps, and returns them with
//! the block it ran at (number, timestamp, parent hash), the hashes of
//! blocks below it and native balances. See `contracts/evm/src/ViewReads.sol`.

use alloy::primitives::{Address, B256, Bytes, U256};
use alloy_sol_types::SolCall;

alloy_sol_types::sol!(
    #[sol(all_derives)]
    "../../contracts/evm/src/ViewReads.sol"
);

pub use ViewReads::{Context, Outcome, Read as Item, Result as ItemResult, readCall, readReturn};

/// The helper's runtime code, generated from its source by
/// `contracts/evm/script/generate-view-code.py`.
pub fn runtime() -> Bytes {
    Bytes::from(
        alloy::hex::decode(include_str!("code/ViewReads.hex").trim())
            .expect("the checked-in helper code is hex"),
    )
}

/// The helper's own limits, as it enforces them.
pub const MAX_READS: usize = 512;
pub const MAX_HASHES: usize = 256;
pub const MAX_RECENT: u64 = 256;
pub const MAX_BALANCES: usize = 256;
pub const MAX_INNER_GAS: u64 = 1_000_000;
pub const MAX_RESULT_BYTES: u64 = 16_384;
pub const MIN_RESULT_BYTES: u64 = 32;
pub const MAX_INPUT_BYTES: u64 = 1_048_576;
pub const MAX_OUTPUT_BYTES: u64 = 1_048_576;

fn round_up_32(bytes: u64) -> u64 {
    bytes.div_ceil(32) * 32
}

/// The most bytes one answer can be, exactly as the helper computes it.
pub fn output_bound(
    reads: usize,
    hashes: usize,
    recent: u64,
    balances: usize,
    result_bytes: u64,
) -> u64 {
    32 * 16
        + 32 * (hashes as u64 + 1)
        + 32 * (recent + 1)
        + 32 * (balances as u64 + 1)
        + 32
        + reads as u64 * (160 + round_up_32(result_bytes))
}

/// The calldata size of one call, exactly as ABI encoding lays it out.
pub fn input_size(reads: &[Item], hashes: usize, balances: usize) -> u64 {
    // Selector, eight head words, then the three dynamic arrays' tails.
    let items: u64 = reads
        .iter()
        .map(|read| 32 + 32 + 32 + 32 + round_up_32(read.data.len() as u64))
        .sum();
    4 + 8 * 32 + (32 + items) + (32 + 32 * hashes as u64) + (32 + 32 * balances as u64)
}

/// One call's arguments.
pub struct Call {
    pub reads: Vec<Item>,
    pub hashes_of: Vec<u64>,
    pub recent: u64,
    pub balances_of: Vec<Address>,
    pub inner_gas: u64,
    pub result_bytes: u64,
    pub input_bytes: u64,
    pub arb_sys: bool,
}

impl Call {
    pub fn encode(&self) -> Bytes {
        readCall {
            reads: self.reads.clone(),
            hashesOf: self.hashes_of.iter().map(|n| U256::from(*n)).collect(),
            recentHashes: U256::from(self.recent),
            balancesOf: self.balances_of.clone(),
            innerGas: U256::from(self.inner_gas),
            resultBytes: U256::from(self.result_bytes),
            inputBytes: U256::from(self.input_bytes),
            arbSys: self.arb_sys,
        }
        .abi_encode()
        .into()
    }

    pub fn output_bound(&self) -> u64 {
        output_bound(
            self.reads.len(),
            self.hashes_of.len(),
            self.recent,
            self.balances_of.len(),
            self.result_bytes,
        )
    }

    /// Decode an answer and check its shape against the call: the counts it
    /// must have and the size it may not pass.
    pub fn decode(&self, answer: &[u8]) -> anyhow::Result<Answer> {
        if answer.len() as u64 > self.output_bound() {
            anyhow::bail!(
                "the helper's answer is {} bytes, above its bound of {}",
                answer.len(),
                self.output_bound()
            );
        }
        let returned = readCall::abi_decode_returns(answer)
            .map_err(|error| anyhow::anyhow!("the helper's answer does not decode: {error}"))?;
        if returned.results.len() != self.reads.len()
            || returned.hashes.len() != self.hashes_of.len()
            || returned.recent.len() as u64 != self.recent
            || returned.balances.len() != self.balances_of.len()
        {
            anyhow::bail!("the helper's answer has other counts than the call asked for");
        }
        let number = u64::try_from(returned.context.number)
            .map_err(|_| anyhow::anyhow!("the helper's block number does not fit"))?;
        let timestamp = u64::try_from(returned.context.timestamp)
            .map_err(|_| anyhow::anyhow!("the helper's block timestamp does not fit"))?;
        Ok(Answer {
            number,
            timestamp,
            parent_hash: returned.context.parentHash,
            gas_at_entry: u64::try_from(returned.context.gasAtEntry).unwrap_or(u64::MAX),
            hashes: returned.hashes,
            recent: returned.recent,
            balances: returned.balances,
            results: returned.results,
        })
    }
}

/// One decoded answer.
pub struct Answer {
    pub number: u64,
    pub timestamp: u64,
    pub parent_hash: B256,
    pub gas_at_entry: u64,
    pub hashes: Vec<B256>,
    pub recent: Vec<B256>,
    pub balances: Vec<U256>,
    pub results: Vec<ItemResult>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::keccak256;

    #[test]
    fn the_helper_code_matches_its_recorded_hash_and_its_sources() {
        let expected: B256 = include_str!("code/ViewReads.hash").trim().parse().unwrap();
        assert_eq!(keccak256(runtime()), expected);
        let sources: std::collections::BTreeMap<String, B256> =
            serde_json::from_str(include_str!("code/sources.json")).unwrap();
        let evm = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contracts/evm");
        assert!(sources.contains_key("src/ViewReads.sol"));
        for (path, hash) in sources {
            let code = std::fs::read(evm.join(&path)).unwrap();
            assert_eq!(
                keccak256(code),
                hash,
                "regenerate the helper code after changing {path}"
            );
        }
    }

    #[test]
    fn the_input_size_is_the_encoded_size() {
        for (count, size) in [(0usize, 0usize), (1, 4), (3, 33), (17, 68), (512, 4)] {
            let reads: Vec<Item> = (0..count)
                .map(|i| Item {
                    target: Address::repeat_byte(i as u8),
                    data: Bytes::from(vec![7u8; size + i % 5]),
                })
                .collect();
            for (hashes, balances) in [(0usize, 0usize), (3, 2), (256, 256)] {
                let call = Call {
                    reads: reads.clone(),
                    hashes_of: (0..hashes as u64).collect(),
                    recent: 256,
                    balances_of: vec![Address::ZERO; balances],
                    inner_gas: 1,
                    result_bytes: 32,
                    input_bytes: 0,
                    arb_sys: false,
                };
                assert_eq!(
                    call.encode().len() as u64,
                    input_size(&reads, hashes, balances)
                );
            }
        }
    }

    #[test]
    fn an_answer_is_bounded_by_the_output_bound() {
        // The largest answer the helper can give for a call fits the bound.
        let call = Call {
            reads: vec![
                Item {
                    target: Address::ZERO,
                    data: Bytes::new(),
                };
                3
            ],
            hashes_of: vec![1, 2],
            recent: 4,
            balances_of: vec![Address::ZERO; 2],
            inner_gas: 1,
            result_bytes: 100,
            input_bytes: 0,
            arb_sys: false,
        };
        let largest = readReturn {
            context: Context {
                number: U256::from(9),
                timestamp: U256::from(9),
                parentHash: B256::repeat_byte(1),
                gasAtEntry: U256::from(9),
            },
            hashes: vec![B256::ZERO; 2],
            recent: vec![B256::ZERO; 4],
            balances: vec![U256::ZERO; 2],
            results: vec![
                ItemResult {
                    outcome: Outcome::Returned,
                    data: Bytes::from(vec![1u8; 100]),
                };
                3
            ],
        };
        let encoded = readCall::abi_encode_returns(&largest);
        assert!(encoded.len() as u64 <= call.output_bound());
        let answer = call.decode(&encoded).unwrap();
        assert_eq!(answer.number, 9);
        // One count off is refused.
        let mut short = largest.clone();
        short.recent.pop();
        assert!(call.decode(&readCall::abi_encode_returns(&short)).is_err());
        // One byte over the bound is refused before decoding.
        let mut padded = encoded.clone();
        padded.resize(call.output_bound() as usize + 1, 0);
        assert!(call.decode(&padded).is_err());
    }
}
