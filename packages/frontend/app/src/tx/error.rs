//! Everything that can stop a flow, and how each is said: in words, in the
//! place the action is (Handoff words: say what happened, then what to do).

use alloy_primitives::{B256, U256};
use alloy_sol_types::SolError;
use chain::bindings::vault::IVault;

use crate::money::{Rounding, format};

/// Why a flow stopped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TxError {
    /// The person declined in their wallet (EIP-1193 code 4001).
    Rejected,
    /// The wallet is locked, or hasn't given this site the account (4100).
    Locked,
    /// The wallet already has a request from this site waiting (-32002).
    AlreadyAsking,
    /// The wallet is on another chain than the vault.
    WrongNetwork { wallet: u64, vault: u64 },
    /// The wallet's account changed since the flow began: it is cancelled
    /// rather than sent from someone else.
    AccountChanged,
    /// Nothing is connected.
    NotConnected,
    /// The person cancelled before anything was sent.
    Cancelled,
    /// More than the wallet holds of the dollar on the vault's chain.
    InsufficientBalance { have: U256 },
    /// More shares than the wallet holds.
    InsufficientShares { have: U256 },
    /// Not enough of the chain's coin for the network fee.
    InsufficientGas { need: U256, have: U256 },
    /// Under the vault's minimum deposit.
    BelowMinimum { minimum: U256 },
    /// Nothing typed, or zero.
    NothingToSend,
    /// The vault isn't in a state that takes this.
    WrongState,
    /// The chain would refuse it: the transaction would revert. Nothing was
    /// sent.
    WouldRevert { reason: Revert },
    /// Mined, and reverted: nothing moved but the network fee.
    Reverted { hash: B256, reason: Option<Revert> },
    /// Another transaction took its place (sped up or cancelled in the
    /// wallet).
    Replaced { hash: B256 },
    /// Sent, and the network we read through never saw it while the app
    /// watched. Another node may have it, so it may still land.
    Dropped { hash: B256 },
    /// Sent, known to the network, and still not mined after a long wait.
    Stuck { hash: B256 },
    /// The digest the app would ask for doesn't match the one the vault
    /// checks: nothing is signed.
    StatementMismatch,
    /// The vault's restricted list read just before signing (`now`) isn't
    /// the one the review showed: nothing is signed, and the person reviews
    /// the deposit again with this list.
    ListChanged { now: String },
    /// The wallet returned a signature that isn't the connected account's
    /// over the statement shown.
    BadSignature,
    /// The chain endpoint answers for another chain than the vault's.
    WrongEndpoint { endpoint: u64, vault: u64 },
    /// The chain or the wallet answered with something unexpected, or not at
    /// all.
    Network(String),
    /// Anything else the wallet said.
    Wallet(String),
    /// The transaction that moves the money was mined (`hash`), and the app
    /// couldn't follow it to the end: the money moved whatever comes next. A
    /// request (`request`) stands and is settled at its price by the keeper,
    /// or by anyone. `then` is what stopped the app: something it couldn't
    /// read, or, when `None`, the long wait running out.
    Landed {
        hash: B256,
        request: Option<U256>,
        then: Option<Box<TxError>>,
    },
    /// The deposit stopped before it was sent, or was refused when mined
    /// (`then`), after its approval of `amount` to the vault was mined: the
    /// approval stands, and depositing the same amount again uses it.
    ApprovalStands { amount: U256, then: Box<TxError> },
}

/// A contract's reason for refusing, as far as it can be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Revert {
    /// One of the vault's own errors, by name.
    Vault(&'static str),
    /// A token's or the chain's message.
    Message(String),
    /// No reason given.
    Unknown,
}

impl Revert {
    /// Read the revert data a node returns (`0x` + selector + arguments).
    pub fn from_data(data: &[u8]) -> Self {
        if data.len() >= 4 {
            let selector: [u8; 4] = data[..4].try_into().unwrap_or_default();
            for (known, name) in VAULT_ERRORS {
                if selector == known {
                    return Self::Vault(name);
                }
            }
        }
        match alloy_sol_types::decode_revert_reason(data) {
            Some(reason) if !reason.is_empty() => {
                Self::Message(reason.trim_start_matches("revert: ").to_string())
            }
            _ => Self::Unknown,
        }
    }

    /// From a node's error message alone, when it carries no data.
    pub fn from_message(message: &str) -> Self {
        for (_, name) in VAULT_ERRORS {
            if message.contains(name) {
                return Self::Vault(name);
            }
        }
        let trimmed = message
            .trim_start_matches("execution reverted")
            .trim_start_matches(':')
            .trim();
        if trimmed.is_empty() {
            Self::Unknown
        } else {
            Self::Message(trimmed.to_string())
        }
    }

    /// What it means, in a sentence.
    pub fn words(&self) -> String {
        match self {
            Self::Vault("WrongState") => {
                "The vault isn't taking this right now: it may have closed or stopped.".to_string()
            }
            Self::Vault("TooSmall") => "That's under the vault's minimum.".to_string(),
            Self::Vault("NoStatement") | Self::Vault("BadSignature") => {
                "The vault needs your signed statement for its current list first.".to_string()
            }
            Self::Vault("NotAllowed") => {
                "Only the owner can do that, or it isn't open to anyone yet.".to_string()
            }
            Self::Vault("NotYet") => "It isn't time for that yet.".to_string(),
            Self::Vault("Nothing") => "There's nothing to collect.".to_string(),
            Self::Vault("Unsettled") => {
                "The vault is settling a valuation. Try again in a few seconds.".to_string()
            }
            Self::Vault(name) => format!("The vault refused it ({name})."),
            Self::Message(message) if message.to_ascii_lowercase().contains("balance") => {
                "That's more than the wallet holds.".to_string()
            }
            Self::Message(message) if message.to_ascii_lowercase().contains("allowance") => {
                "The vault wasn't allowed to take that amount.".to_string()
            }
            Self::Message(message) => format!("The chain refused it: {message}"),
            Self::Unknown => "The chain refused it.".to_string(),
        }
    }
}

/// The vault's errors by selector, from the contract's own interface.
const VAULT_ERRORS: [([u8; 4], &str); 14] = [
    (IVault::WrongState::SELECTOR, "WrongState"),
    (IVault::NotAllowed::SELECTOR, "NotAllowed"),
    (IVault::TooSmall::SELECTOR, "TooSmall"),
    (IVault::NoStatement::SELECTOR, "NoStatement"),
    (IVault::BadSignature::SELECTOR, "BadSignature"),
    (IVault::NotBlessed::SELECTOR, "NotBlessed"),
    (IVault::OutsideWindow::SELECTOR, "OutsideWindow"),
    (IVault::Unsettled::SELECTOR, "Unsettled"),
    (IVault::AlreadyValued::SELECTOR, "AlreadyValued"),
    (IVault::NotYet::SELECTOR, "NotYet"),
    (IVault::Nothing::SELECTOR, "Nothing"),
    (IVault::BelowFloor::SELECTOR, "BelowFloor"),
    (IVault::Stale::SELECTOR, "Stale"),
    (IVault::NotTransferable::SELECTOR, "NotTransferable"),
];

/// The names a page needs to put an error into words.
pub struct Context<'a> {
    /// "MetaMask", "your recovery phrase".
    pub wallet: &'a str,
    /// The vault's chain: "Hyperliquid".
    pub chain: &'a str,
    /// The chain the wallet is on, if known: "Base".
    pub wallet_chain: Option<&'a str>,
    /// The dollar: "USDC".
    pub symbol: &'a str,
    pub decimals: u8,
    /// The chain's coin, which pays the network fee: "ETH".
    pub native: &'a str,
}

/// What a page shows for an error: a short title, a line saying what it
/// means and what to do, and whether it is an error at all (a rejection is
/// the person's own choice, said politely; the rest are alerts).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Words {
    pub title: String,
    pub text: String,
    pub alert: bool,
}

impl TxError {
    /// Whether a transaction may have been sent, so the page must not say
    /// "Nothing was sent".
    pub fn after_send(&self) -> bool {
        self.may_have_moved() || matches!(self, Self::Reverted { .. } | Self::ApprovalStands { .. })
    }

    /// Whether money may have moved (or may still move): the page must not
    /// offer to send the same thing again, only to look at where it stands.
    /// A reverted transaction moved nothing but its fee, so it may be tried
    /// again.
    pub fn may_have_moved(&self) -> bool {
        matches!(
            self,
            Self::Landed { .. } | Self::Replaced { .. } | Self::Dropped { .. } | Self::Stuck { .. }
        )
    }

    /// The transaction the error is about, for its link.
    pub fn hash(&self) -> Option<B256> {
        match self {
            Self::Reverted { hash, .. }
            | Self::Replaced { hash }
            | Self::Dropped { hash }
            | Self::Stuck { hash }
            | Self::Landed { hash, .. } => Some(*hash),
            Self::ApprovalStands { then, .. } => then.hash(),
            _ => None,
        }
    }

    /// Why the app's own second step (settling a priced request) wasn't
    /// sent, as the end of a sentence: the request stands either way.
    pub fn unsent_words(&self, context: &Context<'_>) -> String {
        let wallet = context.wallet;
        match self {
            Self::Rejected => format!("you declined it in {wallet}"),
            Self::Cancelled => "it was stopped before it was sent".to_string(),
            Self::WrongNetwork { .. } => match context.wallet_chain {
                Some(on) => format!("your wallet is on {on}, not {}", context.chain),
                None => format!("your wallet isn’t on {}", context.chain),
            },
            Self::AccountChanged => "your wallet’s account changed".to_string(),
            Self::NotConnected => "no wallet is connected".to_string(),
            Self::Locked => "your wallet is locked".to_string(),
            Self::AlreadyAsking => format!("{wallet} was already asking something else"),
            Self::InsufficientGas { .. } => format!(
                "your wallet needs a little {} on {} for its network fee",
                context.native, context.chain
            ),
            Self::WouldRevert { .. } => "the chain wouldn’t take it just now".to_string(),
            Self::Reverted { .. } => "the chain refused it when it was mined".to_string(),
            Self::Replaced { .. } | Self::Dropped { .. } | Self::Stuck { .. } => {
                "it hasn’t been seen mined".to_string()
            }
            _ => "the network didn’t answer".to_string(),
        }
    }

    pub fn words(&self, context: &Context<'_>) -> Words {
        let wallet = context.wallet;
        let polite = |title: &str, text: String| Words {
            title: title.to_string(),
            text,
            alert: false,
        };
        let alert = |title: &str, text: String| Words {
            title: title.to_string(),
            text,
            alert: true,
        };
        match self {
            Self::Rejected => polite(
                "Rejected in wallet. Nothing was sent.",
                format!("You declined in {wallet}. No money moved and no request was made."),
            ),
            Self::Cancelled => polite("Cancelled. Nothing was sent.", "No money moved.".to_string()),
            Self::Locked => alert(
                "Your wallet is locked",
                format!("Unlock {wallet} and try again. Nothing was sent."),
            ),
            Self::AlreadyAsking => alert(
                "Your wallet is already asking",
                format!("Open {wallet} to answer the request that's waiting there."),
            ),
            Self::WrongNetwork { .. } => alert(
                "Wrong network",
                match context.wallet_chain {
                    Some(on) => format!(
                        "Your wallet is on {on}. This vault is on {}. Switch to {} and try again. Nothing was sent.",
                        context.chain, context.chain
                    ),
                    None => format!(
                        "This vault is on {}. Switch your wallet to {} and try again. Nothing was sent.",
                        context.chain, context.chain
                    ),
                },
            ),
            Self::AccountChanged => alert(
                "Your wallet's account changed",
                "This was set up for another account, so it stopped. Nothing was sent from the new one."
                    .to_string(),
            ),
            Self::NotConnected => alert(
                "No wallet connected",
                "Connect a wallet to continue. Nothing was sent.".to_string(),
            ),
            Self::InsufficientBalance { have } => alert(
                "More than your wallet holds",
                format!(
                    "You have {} on {}.",
                    format::token_balance(*have, context.decimals, context.symbol),
                    context.chain
                ),
            ),
            Self::InsufficientShares { have } => alert(
                "More shares than you hold",
                format!(
                    "You hold {} shares.",
                    format::shares(*have, Rounding::Down)
                ),
            ),
            Self::InsufficientGas { .. } => alert(
                "Not enough for the network fee",
                format!(
                    "Your wallet needs a little {} on {} to pay the network fee. Nothing was sent.",
                    context.native, context.chain
                ),
            ),
            Self::BelowMinimum { minimum } => alert(
                "Below the minimum",
                format!(
                    "The smallest deposit is {}.",
                    format::dollars_exact(*minimum, context.decimals).replace(".00", "")
                ),
            ),
            Self::NothingToSend => alert("Type an amount", "Nothing was sent.".to_string()),
            Self::WrongState => alert(
                "Not possible right now",
                "The vault isn't taking this in its current state. Nothing was sent.".to_string(),
            ),
            Self::WouldRevert { reason } => alert(
                "The network wouldn't accept it. Nothing moved.",
                format!("{} Nothing was sent.", reason.words()),
            ),
            Self::Reverted { reason, .. } => alert(
                "Transaction failed",
                format!(
                    "{} Nothing moved except the network fee.",
                    reason
                        .as_ref()
                        .map(Revert::words)
                        .unwrap_or_else(|| "The chain didn't accept it.".to_string())
                ),
            ),
            Self::Replaced { .. } => alert(
                "Your wallet replaced this transaction",
                format!(
                    "{wallet} sent another transaction in its place. Check your wallet's activity before trying again."
                ),
            ),
            Self::Stuck { .. } => alert(
                "Still waiting to be mined",
                format!(
                    "The network has your transaction but hasn't mined it yet. It may still go through: check {wallet}'s activity before sending anything again."
                ),
            ),
            Self::Dropped { .. } => alert(
                "Not seen by the network yet",
                format!(
                    "The network this app reads hasn't seen your transaction. It may still go through: check {wallet}'s activity before sending anything again."
                ),
            ),
            Self::StatementMismatch => alert(
                "The statement didn't match the vault's",
                "The app stopped before asking you to sign. Nothing was signed or sent.".to_string(),
            ),
            Self::ListChanged { now } => {
                let codes = crate::components::exit::list_codes(now);
                let who = match crate::components::exit::not_open_to(&codes) {
                    Some(words) => format!("It's now not open to: {words}"),
                    None => "It's now open to everyone".to_string(),
                };
                alert(
                    "The vault's list changed since your review",
                    format!(
                        "The vault's list of who it isn't open to changed since you reviewed your deposit. {who} (“{}”). Review your deposit again with the new list. Nothing was signed or sent.",
                        crate::components::exit::restricted_line(now)
                    ),
                )
            }
            Self::BadSignature => alert(
                "The signature didn't match",
                format!("{wallet} signed with another account or other content. Nothing was sent."),
            ),
            Self::WrongEndpoint { .. } => alert(
                "The network didn't answer as expected",
                "The chain's endpoint answered for another chain. Nothing was sent.".to_string(),
            ),
            Self::Network(why) => alert(
                "The network didn't answer",
                format!("{why}. Check your connection and try again."),
            ),
            Self::Wallet(why) => alert("Your wallet refused it", format!("{wallet} said: {why}")),
            Self::ApprovalStands { amount, then } => {
                let words = then.words(context);
                // The inner words say nothing moved; the approval did.
                let reason = [
                    "Nothing was sent.",
                    "No money moved and no request was made.",
                    "No money moved.",
                ]
                .iter()
                .find_map(|said| words.text.strip_suffix(said))
                .map(|text| format!("{text}The deposit wasn't sent."))
                .unwrap_or(words.text);
                let amount = format::dollars_exact(*amount, context.decimals);
                // Its own title: the approval went, so no title of a step
                // that sent nothing stands above it.
                alert(
                    "Not deposited: your approval stands",
                    format!(
                        "{reason} Your approval of {amount} to the vault went through and stands: depositing {amount} again uses it, without asking for it again."
                    ),
                )
            }
            Self::Landed { request, then, .. } => {
                let then = match then.as_deref() {
                    None => "It isn't settled yet, and this page has stopped watching it.",
                    Some(_) => "This page couldn't read what happened next from the network.",
                };
                match request {
                    Some(_) => polite(
                        "Your request stands",
                        format!(
                            "It's on the chain, and it's settled at its price by the keeper, or by anyone. {then} Your portfolio shows it; there's no need to send it again."
                        ),
                    ),
                    None => polite(
                        "It went through",
                        format!(
                            "The transaction was mined. {then} Your portfolio shows the result; there's no need to send it again."
                        ),
                    ),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> Context<'static> {
        Context {
            wallet: "MetaMask",
            chain: "Hyperliquid",
            wallet_chain: Some("Base"),
            symbol: "USDC",
            decimals: 6,
            native: "HYPE",
        }
    }

    #[test]
    fn a_rejection_says_nothing_was_sent_in_the_designs_words_and_is_not_an_alert() {
        let words = TxError::Rejected.words(&context());
        assert_eq!(words.title, "Rejected in wallet. Nothing was sent.");
        assert_eq!(
            words.text,
            "You declined in MetaMask. No money moved and no request was made."
        );
        assert!(!words.alert);
    }

    #[test]
    fn an_insufficient_balance_names_the_balance_on_the_vaults_chain() {
        let words = TxError::InsufficientBalance {
            have: U256::from(812_409_999u64),
        }
        .words(&context());
        assert_eq!(words.text, "You have 812.40 USDC on Hyperliquid.");
        assert!(words.alert);
    }

    #[test]
    fn the_minimum_is_said_in_whole_dollars() {
        let words = TxError::BelowMinimum {
            minimum: U256::from(10_000_000u64),
        }
        .words(&context());
        assert_eq!(words.text, "The smallest deposit is $10.");
    }

    #[test]
    fn a_vault_error_is_read_from_its_selector() {
        let data = IVault::NoStatement::SELECTOR.to_vec();
        assert_eq!(Revert::from_data(&data), Revert::Vault("NoStatement"));
        let data = IVault::WrongState::SELECTOR.to_vec();
        assert_eq!(Revert::from_data(&data), Revert::Vault("WrongState"));
    }

    #[test]
    fn a_string_revert_is_read_as_its_message() {
        let data =
            alloy_sol_types::Revert::from("ERC20: transfer amount exceeds balance").abi_encode();
        let reason = Revert::from_data(&data);
        assert_eq!(
            reason,
            Revert::Message("ERC20: transfer amount exceeds balance".to_string())
        );
        assert_eq!(reason.words(), "That's more than the wallet holds.");
        assert_eq!(Revert::from_data(&[]), Revert::Unknown);
        assert_eq!(Revert::from_message("execution reverted"), Revert::Unknown);
        assert_eq!(
            Revert::from_message("execution reverted: custom error WrongState()"),
            Revert::Vault("WrongState")
        );
    }

    /// Every error a flow can end with once its money-moving transaction is
    /// mined, in words: none may say nothing was sent or invite a re-send.
    #[test]
    fn once_the_money_moved_no_words_say_nothing_was_sent_or_invite_the_same_send_again() {
        let hash = B256::repeat_byte(7);
        let causes = [
            None,
            Some(TxError::Network(
                "the chain answered a read with nothing".to_string(),
            )),
            Some(TxError::Rejected),
            Some(TxError::WrongNetwork {
                wallet: 1,
                vault: 2,
            }),
        ];
        for request in [Some(U256::from(9u8)), None] {
            for then in causes.clone() {
                let error = TxError::Landed {
                    hash,
                    request,
                    then: then.map(Box::new),
                };
                let words = error.words(&context());
                let said = format!("{} {}", words.title, words.text);
                assert!(error.after_send() && error.may_have_moved());
                assert_eq!(error.hash(), Some(hash));
                for never in [
                    "Nothing was sent",
                    "No money moved",
                    "no request was made",
                    "try again",
                    "Try again",
                ] {
                    assert!(!said.contains(never), "{said}");
                }
                assert!(said.contains("no need to send it again"), "{said}");
            }
        }
    }

    #[test]
    fn a_transaction_not_seen_yet_is_never_called_one_that_moved_nothing() {
        let hash = B256::repeat_byte(7);
        for error in [
            TxError::Dropped { hash },
            TxError::Stuck { hash },
            TxError::Replaced { hash },
        ] {
            let words = error.words(&context());
            let said = format!("{} {}", words.title, words.text);
            assert!(error.may_have_moved());
            assert!(!said.contains("nothing moved"), "{said}");
            assert!(!said.contains("You can try again"), "{said}");
        }
        // A revert moved nothing but its fee: that one may be tried again.
        assert!(!TxError::Reverted { hash, reason: None }.may_have_moved());
    }

    #[test]
    fn the_second_step_not_sent_is_said_as_why_with_the_wallets_and_chains_names() {
        assert_eq!(
            TxError::Rejected.unsent_words(&context()),
            "you declined it in MetaMask"
        );
        assert_eq!(
            TxError::WrongNetwork {
                wallet: 8453,
                vault: 998
            }
            .unsent_words(&context()),
            "your wallet is on Base, not Hyperliquid"
        );
        assert_eq!(
            TxError::InsufficientGas {
                need: U256::from(2u8),
                have: U256::from(1u8)
            }
            .unsent_words(&context()),
            "your wallet needs a little HYPE on Hyperliquid for its network fee"
        );
    }

    #[test]
    fn a_reverted_second_step_is_said_once_not_as_didnt_go_through_twice() {
        let words = TxError::Reverted {
            hash: B256::repeat_byte(7),
            reason: None,
        }
        .unsent_words(&context());
        assert!(!words.contains("didn’t go through"), "{words}");
    }

    #[test]
    fn only_errors_after_a_send_carry_a_transaction() {
        let hash = B256::repeat_byte(7);
        assert!(TxError::Reverted { hash, reason: None }.after_send());
        assert_eq!(TxError::Dropped { hash }.hash(), Some(hash));
        assert!(!TxError::Rejected.after_send());
        assert!(
            !TxError::WouldRevert {
                reason: Revert::Unknown
            }
            .after_send()
        );
    }
}
