//! An approval seen mined by an earlier try is taken as used only when the
//! allowance read at a block the endpoint reported, at or past the
//! approval's, shows it short: a pooled endpoint whose latest reads lag
//! behind its reported head never asks for a second approval.

use std::cell::RefCell;

use alloy_primitives::{Address, B256, Bytes, U256, keccak256};
use alloy_sol_types::{SolCall, SolValue};
use app::tx::error::TxError;
use app::tx::flow::{self, Approval, Cancel, Ctx, Nudge, Pace, Prompt, Step};
use app::tx::port::{Clock, Log, Pending, Reader, Receipt, Signer, StatementRequest, TxRequest};
use chain::bindings::erc20::IERC20;
use chain::bindings::vault::{self as b, IVault};
use futures::executor::block_on;

const CHAIN: u64 = 31337;
const USD: u64 = 1_000_000;

fn vault() -> Address {
    Address::repeat_byte(0xaa)
}
fn asset() -> Address {
    Address::repeat_byte(0xdd)
}
fn account() -> Address {
    Address::repeat_byte(0x42)
}
fn list() -> Vec<B256> {
    vec![B256::repeat_byte(1), B256::repeat_byte(2)]
}

fn ok<T: SolValue>(value: T) -> Result<Bytes, TxError> {
    Ok(Bytes::from(value.abi_encode()))
}

/// An endpoint that is a pool of nodes: `eth_blockNumber` is answered by a
/// node at the chain's head (block 200, past the approval's block 101),
/// while `eth_call` at the latest block lands on a node a few blocks behind
/// that hasn't seen the approval (reads 0). On the chain the approval of
/// $1,000 stands, unspent. A read at a block is answered by a node that has
/// it (`has_blocks`), or fails.
struct Pool {
    has_blocks: bool,
}

impl Reader for Pool {
    async fn chain_id(&self) -> Result<u64, TxError> {
        Ok(CHAIN)
    }
    async fn call(&self, to: Address, data: Bytes) -> Result<Bytes, TxError> {
        let selector: [u8; 4] = data[..4].try_into().unwrap();
        if to == asset() {
            return match selector {
                IERC20::balanceOfCall::SELECTOR => ok(U256::from(10_000 * USD)),
                // The lagging node: the approval isn't there yet.
                IERC20::allowanceCall::SELECTOR => ok(U256::ZERO),
                _ => panic!("unexpected dollar read"),
            };
        }
        match selector {
            IVault::stateCall::SELECTOR => ok(b::VaultState::Open),
            IVault::feesCall::SELECTOR => ok(b::Fees {
                holdingBps: 100,
                authorBps: 1000,
                platformBps: 500,
                transactionBps: 10,
            }),
            IVault::timersCall::SELECTOR => ok(b::Timers {
                slotInterval: 10,
                slotLead: 5,
                slotWindow: 3,
                postDeadline: 60,
                transitTimeout: 3600,
                fallbackAfter: 86_400,
                usageFreshness: 3600,
                tankEscapeDelay: 86_400,
                floorDelay: 86_400,
                haltGrace: 600,
                freezeAfter: 604_800,
                windDown: 604_800,
                activationWindow: 259_200,
            }),
            IVault::cashCall::SELECTOR => ok(U256::from(1_000 * USD)),
            IVault::totalSupplyCall::SELECTOR => {
                ok(U256::from(1_000u64) * U256::from(10u64).pow(U256::from(18u8)))
            }
            IVault::minimumDepositCall::SELECTOR => ok(U256::from(10 * USD)),
            IVault::readyToCloseCall::SELECTOR => ok(false),
            IVault::lastPricedCall::SELECTOR => ok((false, U256::ZERO, 0u64)),
            // The statement for the list as it stands is on record.
            IVault::statementOfCall::SELECTOR => ok(keccak256(list().abi_encode())),
            IVault::restrictedCall::SELECTOR => ok(list()),
            IVault::restrictedTextCall::SELECTOR => ok("SANCTIONED,US".to_string()),
            other => panic!("unexpected vault read {other:?}"),
        }
    }
    async fn call_at(&self, to: Address, data: Bytes, block: u64) -> Result<Bytes, TxError> {
        if !self.has_blocks || block > 200 {
            return Err(TxError::Network("header not found".to_string()));
        }
        if to == asset() && data[..4] == IERC20::allowanceCall::SELECTOR {
            return ok(U256::from(1_000 * USD));
        }
        self.call(to, data).await
    }
    async fn native_balance(&self, _who: Address) -> Result<U256, TxError> {
        Ok(U256::from(10u64).pow(U256::from(18u8)))
    }
    async fn estimate_gas(&self, _request: &TxRequest) -> Result<u64, TxError> {
        Ok(100_000)
    }
    async fn gas_price(&self) -> Result<U256, TxError> {
        Ok(U256::from(1_000_000_000u64))
    }
    async fn receipt(&self, _hash: B256) -> Result<Option<Receipt>, TxError> {
        Ok(None)
    }
    async fn pending(&self, _hash: B256) -> Result<Pending, TxError> {
        Ok(Pending::Unknown)
    }
    async fn mined_nonce(&self, _who: Address) -> Result<u64, TxError> {
        Ok(0)
    }
    async fn chain_time(&self) -> Result<u64, TxError> {
        Ok(1_800_000_000)
    }
    async fn block_number(&self) -> Result<Option<u64>, TxError> {
        Ok(Some(200))
    }
    async fn block_time(&self, _block: u64) -> Result<u64, TxError> {
        Ok(1_800_000_000)
    }
    async fn logs(
        &self,
        _address: Address,
        _topic0: B256,
        _topic1: Option<B256>,
        _from_block: u64,
    ) -> Result<Vec<Log>, TxError> {
        Ok(Vec::new())
    }
}

/// A wallet that declines whatever it is asked to send.
struct Declines;

impl Signer for Declines {
    fn account(&self) -> Option<Address> {
        Some(account())
    }
    fn name(&self) -> String {
        "MetaMask".to_string()
    }
    async fn chain_id(&self) -> Result<u64, TxError> {
        Ok(CHAIN)
    }
    async fn sign_statement(&self, _statement: &StatementRequest) -> Result<Bytes, TxError> {
        panic!("no statement is needed: one is on record")
    }
    async fn send(&self, _request: &TxRequest) -> Result<B256, TxError> {
        Err(TxError::Rejected)
    }
}

struct NoWait;
impl Clock for NoWait {
    async fn sleep(&self, _millis: u32) {}
}

/// A $1,000 approval mined in block 101 by an earlier try stands unspent on
/// the chain. The endpoint's head is at 200, while its latest allowance read
/// comes from a node that lags and reads 0: the allowance is read at block
/// 200 instead, and covers the deposit (or that read fails, which is no
/// evidence it was used). No second approval is asked for.
#[test]
fn a_pooled_endpoint_whose_head_is_past_the_approval_but_whose_latest_allowance_read_lags_asks_no_second_approval()
 {
    for has_blocks in [true, false] {
        let steps = RefCell::new(Vec::new());
        let on_step = |step: Step| steps.borrow_mut().push(step);
        let ctx = Ctx {
            reader: &Pool { has_blocks },
            signer: &Declines,
            clock: &NoWait,
            chain_id: CHAIN,
            vault: vault(),
            asset: asset(),
            account: account(),
            cancel: Cancel::new(),
            settle_now: Nudge::new(),
            skip_second: Nudge::new(),
            pace: Pace::FAST,
            on_step: &on_step,
        };
        let known = Approval {
            amount: U256::from(1_000 * USD),
            block: 101,
        };
        let _ = block_on(flow::deposit_after(
            &ctx,
            U256::from(1_000 * USD),
            "SANCTIONED,US",
            Some(known),
        ));
        let steps = steps.into_inner();
        assert!(
            !steps
                .iter()
                .any(|step| matches!(step, Step::Signing(Prompt::Approve { .. }))),
            "a second approval was asked for while the mined one covers the deposit ({has_blocks}): {steps:?}"
        );
        // The deposit itself was offered, on the approval.
        assert!(
            steps
                .iter()
                .any(|step| matches!(step, Step::Signing(Prompt::Deposit { .. }))),
            "{steps:?}"
        );
    }
}
