//! The first fallback when an endpoint refuses state overrides: Multicall3
//! at its canonical address, used only where its code is the known code.
//! One `aggregate3` gives the same content as the helper: the block number
//! (from ArbSys on an Orbit chain, where Multicall3's own is the parent
//! chain's), the timestamp, block hashes, balances and every read, each
//! allowed to fail on its own.

use alloy::primitives::{Address, B256, Bytes, U256, address, b256};
use alloy_sol_types::SolCall;

use super::Read;
use super::capabilities::{ARB_SYS, BlockSource};

alloy_sol_types::sol! {
    /// Multicall3's functions the follower uses.
    interface IMulticall3 {
        struct Call3 {
            address target;
            bool allowFailure;
            bytes callData;
        }
        struct Result {
            bool success;
            bytes returnData;
        }
        function aggregate3(Call3[] calldata calls) external payable returns (Result[] memory returnData);
        function getBlockNumber() external view returns (uint256 blockNumber);
        function getCurrentBlockTimestamp() external view returns (uint256 timestamp);
        function getBlockHash(uint256 blockNumber) external view returns (bytes32 blockHash);
        function getEthBalance(address addr) external view returns (uint256 balance);
    }

    /// The ArbSys precompile's block functions.
    interface IArbSys {
        function arbBlockNumber() external view returns (uint256);
        function arbBlockHash(uint256 arbBlockNum) external view returns (bytes32);
    }
}

/// Multicall3's canonical address.
pub const MULTICALL3: Address = address!("0xcA11bde05977b3631167028862bE2a173976CA11");

/// The hash of Multicall3's canonical runtime code.
pub const MULTICALL3_CODE_HASH: B256 =
    b256!("0xd5c15df687b16f2ff992fc8d767b4216323184a2bbc6ee2f9c398c318e770891");

/// One `aggregate3` and where each part of its answer is.
pub struct Batch {
    pub calls: Vec<IMulticall3::Call3>,
    pub hash_numbers: Vec<u64>,
    pub balances: usize,
    pub reads: usize,
}

fn call3(target: Address, data: Vec<u8>) -> IMulticall3::Call3 {
    IMulticall3::Call3 {
        target,
        allowFailure: true,
        callData: Bytes::from(data),
    }
}

impl Batch {
    /// The number and timestamp first, then a hash for each of
    /// `hash_numbers`, a balance for each of `balances`, and `reads`.
    pub fn new(
        multicall3: Address,
        source: BlockSource,
        hash_numbers: Vec<u64>,
        balances: &[Address],
        reads: &[Read],
    ) -> Self {
        let mut calls = Vec::with_capacity(2 + hash_numbers.len() + balances.len() + reads.len());
        calls.push(match source {
            BlockSource::Plain => {
                call3(multicall3, IMulticall3::getBlockNumberCall {}.abi_encode())
            }
            BlockSource::ArbSys => call3(ARB_SYS, IArbSys::arbBlockNumberCall {}.abi_encode()),
        });
        calls.push(call3(
            multicall3,
            IMulticall3::getCurrentBlockTimestampCall {}.abi_encode(),
        ));
        for number in &hash_numbers {
            calls.push(match source {
                BlockSource::Plain => call3(
                    multicall3,
                    IMulticall3::getBlockHashCall {
                        blockNumber: U256::from(*number),
                    }
                    .abi_encode(),
                ),
                BlockSource::ArbSys => call3(
                    ARB_SYS,
                    IArbSys::arbBlockHashCall {
                        arbBlockNum: U256::from(*number),
                    }
                    .abi_encode(),
                ),
            });
        }
        for of in balances {
            calls.push(call3(
                multicall3,
                IMulticall3::getEthBalanceCall { addr: *of }.abi_encode(),
            ));
        }
        for read in reads {
            calls.push(call3(read.target, read.data.to_vec()));
        }
        Self {
            calls,
            hash_numbers,
            balances: balances.len(),
            reads: reads.len(),
        }
    }

    pub fn encode(&self) -> Bytes {
        IMulticall3::aggregate3Call {
            calls: self.calls.clone(),
        }
        .abi_encode()
        .into()
    }

    /// Split an answer into its parts. A hash call that failed (ArbSys
    /// refuses blocks out of its window) is a zero hash.
    pub fn decode(&self, answer: &[u8]) -> anyhow::Result<Parts> {
        let results = IMulticall3::aggregate3Call::abi_decode_returns(answer).map_err(|error| {
            anyhow::anyhow!(
                "Multicall3's answer ({} bytes) does not decode: {error}",
                answer.len()
            )
        })?;
        if results.len() != self.calls.len() {
            anyhow::bail!("Multicall3's answer has another count than the call asked for");
        }
        let word = |result: &IMulticall3::Result| -> Option<U256> {
            (result.success && result.returnData.len() == 32)
                .then(|| U256::from_be_slice(&result.returnData))
        };
        let number = word(&results[0])
            .and_then(|number| u64::try_from(number).ok())
            .ok_or_else(|| anyhow::anyhow!("Multicall3 gave no block number"))?;
        let timestamp = word(&results[1])
            .and_then(|time| u64::try_from(time).ok())
            .ok_or_else(|| anyhow::anyhow!("Multicall3 gave no block timestamp"))?;
        let mut at = 2;
        let mut hashes = Vec::with_capacity(self.hash_numbers.len());
        for number in &self.hash_numbers {
            let hash = word(&results[at]).map_or(B256::ZERO, B256::from);
            hashes.push((*number, hash));
            at += 1;
        }
        let mut balances = Vec::with_capacity(self.balances);
        for _ in 0..self.balances {
            balances.push(
                word(&results[at]).ok_or_else(|| anyhow::anyhow!("Multicall3 gave no balance"))?,
            );
            at += 1;
        }
        let reads = results[at..].to_vec();
        Ok(Parts {
            number,
            timestamp,
            hashes,
            balances,
            reads,
        })
    }
}

/// One `aggregate3` answer, split.
pub struct Parts {
    pub number: u64,
    pub timestamp: u64,
    pub hashes: Vec<(u64, B256)>,
    pub balances: Vec<U256>,
    pub reads: Vec<IMulticall3::Result>,
}
