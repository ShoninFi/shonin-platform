//! A deposit whose approval was sent and mined in the same try, and which
//! then stopped because the wallet moved to another account or disconnected
//! before the deposit was offered, remembers the mined approval for the
//! account that approved: the next try from that account, through a pooled
//! endpoint whose latest read lags (allowance 0), doesn't ask for a second
//! approval while the mined one stands unspent on the chain.

use std::cell::{Cell, RefCell};

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

/// The chain (head 200) and a pooled endpoint whose latest `eth_call`
/// lands on a lagging node reading the allowance as 0; reads at a block are
/// answered from the chain's real state.
#[derive(Default)]
struct World {
    /// The allowance on the chain.
    allowance: Cell<u64>,
    receipts: RefCell<Vec<Receipt>>,
    sent: RefCell<Vec<[u8; 4]>>,
    /// The wallet moved to another account once the approval was sent.
    moved: Cell<bool>,
    move_after_approval: Cell<bool>,
    /// Disconnected, rather than moved, once the approval was sent.
    disconnect: Cell<bool>,
    /// The account the approval's receipt says sent it, when not the one
    /// asked for (a wallet that sent from another account).
    signed_by: Cell<Option<Address>>,
}

struct Pool<'a>(&'a World);

impl Reader for Pool<'_> {
    async fn chain_id(&self) -> Result<u64, TxError> {
        Ok(CHAIN)
    }
    async fn call(&self, to: Address, data: Bytes) -> Result<Bytes, TxError> {
        let selector: [u8; 4] = data[..4].try_into().unwrap();
        if to == asset() {
            return match selector {
                IERC20::balanceOfCall::SELECTOR => ok(U256::from(10_000 * USD)),
                // The lagging node.
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
            IVault::statementOfCall::SELECTOR => ok(keccak256(list().abi_encode())),
            IVault::restrictedCall::SELECTOR => ok(list()),
            IVault::restrictedTextCall::SELECTOR => ok("SANCTIONED,US".to_string()),
            other => panic!("unexpected vault read {other:?}"),
        }
    }
    async fn call_at(&self, to: Address, data: Bytes, block: u64) -> Result<Bytes, TxError> {
        if block > 200 {
            return Err(TxError::Network("header not found".to_string()));
        }
        if to == asset() && data[..4] == IERC20::allowanceCall::SELECTOR {
            return ok(U256::from(self.0.allowance.get()));
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
    async fn receipt(&self, hash: B256) -> Result<Option<Receipt>, TxError> {
        Ok(self
            .0
            .receipts
            .borrow()
            .iter()
            .find(|r| r.hash == hash)
            .cloned())
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

/// A wallet that sends an approval (mined in block 101 at once) and, when
/// told to, moves to another account right after; it declines a deposit.
struct Wallet<'a>(&'a World);

impl Signer for Wallet<'_> {
    fn account(&self) -> Option<Address> {
        if self.0.moved.get() && self.0.disconnect.get() {
            return None;
        }
        Some(if self.0.moved.get() {
            Address::repeat_byte(0x77)
        } else {
            account()
        })
    }
    fn name(&self) -> String {
        "MetaMask".to_string()
    }
    async fn chain_id(&self) -> Result<u64, TxError> {
        Ok(CHAIN)
    }
    async fn sign_statement(&self, _statement: &StatementRequest) -> Result<Bytes, TxError> {
        panic!("a statement is on record")
    }
    async fn send(&self, request: &TxRequest) -> Result<B256, TxError> {
        let selector: [u8; 4] = request.data[..4].try_into().unwrap();
        if selector != IERC20::approveCall::SELECTOR {
            return Err(TxError::Rejected);
        }
        let call = IERC20::approveCall::abi_decode(&request.data).unwrap();
        self.0.allowance.set(u64::try_from(call.value).unwrap());
        self.0.sent.borrow_mut().push(selector);
        let hash = B256::with_last_byte(self.0.sent.borrow().len() as u8);
        self.0.receipts.borrow_mut().push(Receipt {
            hash,
            from: Some(self.0.signed_by.get().unwrap_or(request.from)),
            success: true,
            block: 101,
            logs: Vec::new(),
        });
        if self.0.move_after_approval.get() {
            self.0.moved.set(true);
        }
        Ok(hash)
    }
}

struct NoWait;
impl Clock for NoWait {
    async fn sleep(&self, _millis: u32) {}
}

fn run(world: &World, known: Option<Approval>) -> (Result<flow::Outcome, TxError>, Vec<Step>) {
    let steps = RefCell::new(Vec::new());
    let on_step = |step: Step| steps.borrow_mut().push(step);
    let reader = Pool(world);
    let wallet = Wallet(world);
    let ctx = Ctx {
        reader: &reader,
        signer: &wallet,
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
    let result = block_on(flow::deposit_after(
        &ctx,
        U256::from(1_000 * USD),
        "SANCTIONED,US",
        known,
    ));
    (result, steps.into_inner())
}

/// What `wallet::run::start` keeps after a flow: every step, then its end.
fn remembered(
    steps: &[Step],
    result: &Result<flow::Outcome, TxError>,
    known: Option<Approval>,
) -> Option<Approval> {
    let end = match result {
        Ok(done) => Step::Done(done.clone()),
        Err(error) => Step::Failed(error.clone()),
    };
    steps.iter().chain([&end]).fold(known, |known, step| {
        app::wallet::run::approval_after(step, known)
    })
}

fn stops_on_a_wallet_left_then_deposits_again(disconnect: bool) {
    let world = World::default();
    world.move_after_approval.set(true);
    world.disconnect.set(disconnect);
    // First try: no allowance; the $1,000 approval is sent and mined in
    // block 101; the wallet then leaves the account before the deposit.
    let (result, steps) = run(&world, None);
    let left = if disconnect {
        TxError::NotConnected
    } else {
        TxError::AccountChanged
    };
    assert!(
        matches!(&result, Err(TxError::ApprovalStands { amount, block: 101, sent: true, then })
            if *amount == U256::from(1_000 * USD) && **then == left),
        "{result:?} {steps:?}"
    );
    assert_eq!(
        world.allowance.get(),
        1_000 * USD,
        "the approval stands on the chain"
    );
    let kept = remembered(&steps, &result, None);
    assert!(kept.is_some(), "the mined approval was not remembered");

    // The person comes back and deposits again; the latest read still lags.
    world.moved.set(false);
    world.move_after_approval.set(false);
    world.sent.borrow_mut().clear();
    let (_, steps) = run(&world, kept);
    assert!(
        !steps
            .iter()
            .any(|step| matches!(step, Step::Signing(Prompt::Approve { .. }))),
        "kept {kept:?}: a second approval was asked for while the mined $1,000 one stands unspent: {steps:?}"
    );
}

#[test]
fn a_mined_approval_whose_deposit_stopped_on_an_account_change_is_not_asked_for_again() {
    stops_on_a_wallet_left_then_deposits_again(false);
}

#[test]
fn a_mined_approval_whose_deposit_stopped_on_a_disconnect_is_not_asked_for_again() {
    stops_on_a_wallet_left_then_deposits_again(true);
}

/// An approval whose receipt says another account sent it isn't the flow's
/// account's: the stop carries no approval, and none is remembered.
#[test]
fn a_mined_approval_whose_receipt_names_another_sender_is_not_remembered() {
    let world = World::default();
    world.move_after_approval.set(true);
    world.signed_by.set(Some(Address::repeat_byte(0x77)));
    let (result, steps) = run(&world, None);
    assert_eq!(result, Err(TxError::AccountChanged), "{steps:?}");
    assert_eq!(remembered(&steps, &result, None), None);
}
