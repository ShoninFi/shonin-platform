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
    /// Another transaction used its nonce (sped up or cancelled in the
    /// wallet, or, for a send signed in the page, possibly an earlier try
    /// of the same action from this tab or another).
    Replaced { hash: B256 },
    /// Sent, and the network we read through never saw it while the app
    /// watched. Another node may have it, so it may still land.
    Dropped { hash: B256 },
    /// Signed in the page and sent through the endpoint the app reads,
    /// which then showed, for a while, its hash unknown and its nonce
    /// unused. A node behind that endpoint may still hold it, but the page
    /// signs the next send at that same nonce while the chain shows it
    /// unused, so at most one of the two can land.
    NotTaken { hash: B256 },
    /// This transaction was signed in the page and sent, but another one
    /// the page signed at the same nonce (`hash`: an earlier or later try,
    /// of this action or another) was mined instead: this one can't go
    /// through. `refused`: whether the chain refused that other one
    /// (nothing moved but its fee), or `None` when its receipt's outcome
    /// can't be read. Never this action's own outcome.
    OtherTryMined { hash: B256, refused: Option<bool> },
    /// The browser's storage, where the page keeps track of what a wallet
    /// in the page sends, couldn't be read or written: nothing was sent,
    /// since a reload or another tab couldn't see the send and could send
    /// past it.
    StorageNeeded,
    /// Sent, known to the network, and still not mined after a long wait.
    Stuck { hash: B256 },
    /// The digest the app would ask for doesn't match the one the vault
    /// checks: nothing is signed.
    StatementMismatch,
    /// The vault's restricted list read just before signing (`now`) isn't
    /// the one the review showed: nothing is signed, and the person reviews
    /// the deposit again with this list.
    ListChanged { now: String },
    /// The chain's settings the page read (its config: endpoint, dollar,
    /// decimals) were updated since the review was computed: nothing is
    /// signed, and the person reviews again with the new settings.
    SettingsChanged,
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
    /// The vault's simulation refused a deposit, for want of an allowance or
    /// for no reason given, while the endpoint reported a block before the
    /// one its approval was mined in: the approval isn't visible there yet.
    /// Nothing was sent; trying again shortly uses it.
    NotCaughtUp,
    /// The deposit stopped before it was sent, or was refused when mined
    /// (`then`), after its approval of `amount` to the vault was mined (in
    /// `block`): the approval stands, and depositing again uses it. `sent`:
    /// this try sent the approval (otherwise an earlier try did, and its fee
    /// isn't this try's).
    ApprovalStands {
        amount: U256,
        block: u64,
        sent: bool,
        then: Box<TxError>,
    },
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

    /// Whether it may be for want of an allowance: no reason given, or a
    /// token's message about its allowance.
    pub fn may_be_allowance(&self) -> bool {
        match self {
            Self::Unknown => true,
            Self::Message(message) => message.to_ascii_lowercase().contains("allowance"),
            Self::Vault(_) => false,
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
    /// Whether the wallet that sent keeps its own list of what it sent:
    /// decided by its kind ([`crate::wallet::Sender`]), never by its name.
    pub keeps_activity: bool,
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
        self.may_have_moved()
            || matches!(
                self,
                Self::Reverted { .. } | Self::ApprovalStands { .. } | Self::OtherTryMined { .. }
            )
    }

    /// Whether money may have moved (or may still move): the page must not
    /// offer to send the same thing again, only to look at where it stands.
    /// A reverted transaction moved nothing but its fee, so it may be tried
    /// again.
    pub fn may_have_moved(&self) -> bool {
        matches!(
            self,
            Self::Landed { .. }
                | Self::OtherTryMined {
                    refused: None | Some(false),
                    ..
                }
                | Self::Replaced { .. }
                | Self::Dropped { .. }
                | Self::Stuck { .. }
        )
    }

    /// The transaction the error is about, for its link.
    pub fn hash(&self) -> Option<B256> {
        match self {
            Self::Reverted { hash, .. }
            | Self::Replaced { hash }
            | Self::Dropped { hash }
            | Self::NotTaken { hash }
            | Self::OtherTryMined { hash, .. }
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
            Self::InsufficientGas { need, have } => match fee_shortfall(*need, *have, context) {
                Some(amounts) => format!("its network fee needs {amounts}"),
                None => format!(
                    "your wallet needs a little {} on {} for its network fee",
                    context.native, context.chain
                ),
            },
            Self::WouldRevert { .. } => "the chain wouldn’t take it just now".to_string(),
            Self::Reverted { .. } => "the chain refused it when it was mined".to_string(),
            Self::Replaced { .. } | Self::Dropped { .. } | Self::Stuck { .. } => {
                "it hasn’t been seen mined".to_string()
            }
            Self::NotTaken { .. } => "the network didn’t show it taken".to_string(),
            Self::OtherTryMined { .. } => {
                "another try from this page was mined at its place".to_string()
            }
            Self::StorageNeeded => {
                "this browser’s storage, needed to keep track of it, couldn’t be used".to_string()
            }
            _ => "the network didn’t answer".to_string(),
        }
    }

    pub fn words(&self, context: &Context<'_>) -> Words {
        let wallet = context.wallet;
        // A line may start with the wallet's name ("the development
        // wallet …"): every one reads as a sentence.
        let polite = |title: &str, text: String| Words {
            title: crate::format::sentence(title),
            text: crate::format::sentence(&text),
            alert: false,
        };
        let alert = |title: &str, text: String| Words {
            title: crate::format::sentence(title),
            text: crate::format::sentence(&text),
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
            Self::InsufficientGas { need, have } => alert(
                "Not enough for the network fee",
                match fee_shortfall(*need, *have, context) {
                    Some(amounts) => format!(
                        "The network fee needs {amounts}. Add {} to your wallet and try again. Nothing was sent.",
                        context.native
                    ),
                    None => format!(
                        "Your wallet needs a little {} on {} to pay the network fee. Nothing was sent.",
                        context.native, context.chain
                    ),
                },
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
            Self::Replaced { .. } if context.keeps_activity => alert(
                "Your wallet replaced this transaction",
                format!(
                    "{wallet} sent another transaction in its place. Check your wallet's activity before doing anything again."
                ),
            ),
            Self::Replaced { .. } => alert(
                "Another transaction used this nonce",
                format!(
                    "Another transaction from this address used this one's nonce. It may be an earlier try of this same action, from this tab or another. Check your address on the {} explorer before doing anything again.",
                    context.chain
                ),
            ),
            Self::NotTaken { .. } => alert(
                "Not sent, as far as the network shows",
                "The network this app reads didn't show this transaction taken. It's safe to try again: a new try takes this one's place, so at most one of them goes through."
                    .to_string(),
            ),
            Self::StorageNeeded => alert(
                "This browser's storage is needed",
                "This browser's storage is needed to keep track of what this wallet sends, and it couldn't be used just now, so nothing was sent. Allow this site to keep data (a private window or a full browser may not), or use another browser."
                    .to_string(),
            ),
            Self::OtherTryMined { refused, .. } => {
                let (title, how) = match refused {
                    Some(false) => (
                        "Another try from this page went through",
                        " and went through",
                    ),
                    Some(true) => (
                        "Another try from this page was refused",
                        ", and the chain refused it: nothing moved except its network fee",
                    ),
                    None => ("Another try from this page was mined", ""),
                };
                alert(
                    title,
                    format!(
                        "This transaction was sent, but another one this page signed at the same place (an earlier or later try, of this action or another) was mined instead{how}. This one can't go through. Check your address on the {} explorer before doing anything again.",
                        context.chain
                    ),
                )
            }
            Self::Stuck { .. } => alert(
                "Still waiting to be mined",
                format!(
                    "The network has your transaction but hasn't mined it yet. It may still go through: {}.",
                    look_before_sending_again(wallet, context.keeps_activity, context.chain)
                ),
            ),
            Self::Dropped { .. } => alert(
                "Not seen by the network yet",
                format!(
                    "The network this app reads hasn't seen your transaction. It may still go through: {}.",
                    look_before_sending_again(wallet, context.keeps_activity, context.chain)
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
            Self::SettingsChanged => alert(
                "This chain's settings were just updated",
                format!(
                    "The app's settings for {} were updated since you reviewed this, so the figures you saw may not hold. Review it again with the new settings. Nothing was signed or sent.",
                    context.chain
                ),
            ),
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
            Self::NotCaughtUp => alert(
                "The network hasn't caught up yet",
                "The network this app reads hasn't caught up with your approval yet, so it can't check the deposit. Try again in a moment. Nothing was sent."
                    .to_string(),
            ),
            Self::ApprovalStands {
                amount, sent, then, ..
            } => {
                let words = then.words(context);
                let amount_words = format::dollars_exact(*amount, context.decimals);
                // The wallet left the account that approved: the stop is said
                // as it is, and the approval as that account's, not the one
                // now connected.
                if matches!(**then, Self::AccountChanged | Self::NotConnected) {
                    return Words {
                        text: format!(
                            "{} The approval of {amount_words} to the vault from the account that started this went through and stands: depositing {amount_words} from that account again uses it.",
                            words.text
                        ),
                        ..words
                    };
                }
                // The inner words say nothing moved; the approval did. Two
                // fees only when this try sent the approval too.
                let both = sent
                    .then(|| refused_after_approval(&words.text, "deposit"))
                    .flatten();
                let reason = both.unwrap_or_else(|| {
                    [
                        "Nothing was sent.",
                        "No money moved and no request was made.",
                        "No money moved.",
                    ]
                    .iter()
                    .find_map(|said| words.text.strip_suffix(said))
                    .map(|text| format!("{text}The deposit wasn't sent."))
                    .unwrap_or(words.text)
                });
                let amount = amount_words;
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

/// Where to look for a transaction that may still go through before
/// sending anything again, as the end of a sentence: the activity a browser
/// wallet keeps of what it sent (`keeps_activity`), or, for a wallet that
/// signs in the page and keeps no such list (the recovery-phrase and
/// development wallets), the address on the chain's explorer, or waiting.
pub fn look_before_sending_again(wallet: &str, keeps_activity: bool, chain: &str) -> String {
    if keeps_activity {
        format!("check the activity in {wallet} before sending anything again")
    } else {
        format!(
            "check your address on the {chain} explorer, or wait, before sending anything again"
        )
    }
}

/// The words of a step refused when it was mined (`step`: "deposit"),
/// after its approval was mined, when `text` is those of the refusal: two
/// transactions were mined, so two network fees were paid, not one.
pub(crate) fn refused_after_approval(text: &str, step: &str) -> Option<String> {
    text.strip_suffix("Nothing moved except the network fee.")
        .map(|reason| {
            format!(
                "{reason}No money moved. Two network fees were paid: one for the approval and one for the refused {step}."
            )
        })
}

/// The decimals of an EVM chain's coin, which pays the network fee.
const NATIVE_DECIMALS: u8 = 18;

/// The fee needed and the coin held, in the chain's coin: "up to 0.0009218
/// ETH on Base, and your wallet holds 0.0009 ETH". `None` when the amounts
/// aren't known (a wallet that only said its funds were short).
fn fee_shortfall(need: U256, have: U256, context: &Context<'_>) -> Option<String> {
    if need.is_zero() || have >= need {
        return None;
    }
    Some(format!(
        "up to {} on {}, and your wallet holds {}",
        format::token_exact(need, NATIVE_DECIMALS, context.native),
        context.chain,
        format::token_exact(have, NATIVE_DECIMALS, context.native),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> Context<'static> {
        Context {
            wallet: "MetaMask",
            keeps_activity: true,
            chain: "Hyperliquid",
            wallet_chain: Some("Base"),
            symbol: "USDC",
            decimals: 6,
            native: "HYPE",
        }
    }

    #[test]
    fn a_transaction_not_seen_from_a_wallet_in_the_page_points_to_the_explorer_not_a_wallets_activity()
     {
        let hash = B256::repeat_byte(7);
        for wallet in [crate::wallet::PHRASE_WALLET, crate::wallet::DEV_WALLET] {
            let context = Context {
                wallet,
                keeps_activity: false,
                ..context()
            };
            for error in [TxError::Dropped { hash }, TxError::Stuck { hash }] {
                let words = error.words(&context);
                assert!(!words.text.contains("activity"), "{}", words.text);
                assert!(
                    words.text.contains(
                        "It may still go through: check your address on the Hyperliquid explorer, or wait, before sending anything again."
                    ),
                    "{}",
                    words.text
                );
            }
        }
        // A browser wallet keeps its own list of what it sent.
        let words = TxError::Dropped { hash }.words(&context());
        assert!(
            words
                .text
                .contains("check the activity in MetaMask before sending anything again"),
            "{}",
            words.text
        );
    }

    /// Whether the words point to a wallet's activity follows the kind of
    /// wallet that sent, not its name: a browser wallet announcing the
    /// phrase wallet's name still keeps its activity.
    #[test]
    fn the_activity_list_is_pointed_to_by_the_senders_kind_not_its_name() {
        let hash = B256::repeat_byte(7);
        let named_like_the_phrase_wallet = Context {
            wallet: crate::wallet::PHRASE_WALLET,
            ..context()
        };
        let words = TxError::Dropped { hash }.words(&named_like_the_phrase_wallet);
        assert!(
            words.text.contains("check the activity in"),
            "{}",
            words.text
        );
        let in_the_page = Context {
            wallet: "MetaMask",
            keeps_activity: false,
            ..context()
        };
        let words = TxError::Dropped { hash }.words(&in_the_page);
        assert!(!words.text.contains("activity"), "{}", words.text);
    }

    /// A transaction from the phrase wallet replaced at its nonce was
    /// replaced by another transaction from the same address sent from
    /// elsewhere: the words point to the explorer, never to an activity
    /// list, and never say the phrase wallet sent it.
    #[test]
    fn a_replaced_transaction_from_a_wallet_in_the_page_points_to_the_explorer() {
        let context = Context {
            wallet: crate::wallet::PHRASE_WALLET,
            keeps_activity: false,
            ..context()
        };
        let words = TxError::Replaced {
            hash: B256::repeat_byte(7),
        }
        .words(&context);
        assert!(!words.text.contains("activity"), "{}", words.text);
        assert!(
            !words.text.contains(crate::wallet::PHRASE_WALLET),
            "{}",
            words.text
        );
        assert!(
            words.text.contains("used this one's nonce"),
            "{}",
            words.text
        );
        assert!(
            words
                .text
                .contains("an earlier try of this same action, from this tab or another"),
            "{}",
            words.text
        );
        assert!(
            words
                .text
                .contains("Hyperliquid explorer before doing anything again"),
            "{}",
            words.text
        );
        assert!(
            !words.text.contains("try again") && !words.text.contains("trying again"),
            "{}",
            words.text
        );
        assert!(
            TxError::Replaced {
                hash: B256::repeat_byte(7)
            }
            .may_have_moved()
        );
    }

    /// A send the endpoint showed as never taken is said safe to try again,
    /// since a new try takes its place, and never "nothing moved": a node
    /// behind the endpoint may still hold it.
    #[test]
    fn a_transaction_not_shown_taken_is_said_safe_to_try_again_without_claiming_nothing_moved() {
        let error = TxError::NotTaken {
            hash: B256::repeat_byte(7),
        };
        assert!(!error.may_have_moved());
        let words = error.words(&context());
        assert!(words.text.contains("safe to try again"), "{}", words.text);
        assert!(words.text.contains("at most one"), "{}", words.text);
        assert!(!words.text.contains("Nothing moved"), "{}", words.text);
    }

    /// Another try of the page's mined at this one's nonce is said as that
    /// other try, with its link, never as this action's outcome: this one
    /// was sent and can't go through, and the person checks the explorer.
    /// Only one the chain refused is said to have moved nothing.
    #[test]
    fn another_try_mined_at_the_nonce_is_never_said_as_this_actions_outcome() {
        for refused in [Some(false), Some(true), None] {
            let error = TxError::OtherTryMined {
                hash: B256::repeat_byte(7),
                refused,
            };
            assert_eq!(error.may_have_moved(), refused != Some(true));
            assert!(error.after_send());
            assert_eq!(error.hash(), Some(B256::repeat_byte(7)));
            let words = error.words(&context());
            assert!(
                words.title.starts_with("Another try from this page"),
                "{}",
                words.title
            );
            assert!(
                words.text.contains("This transaction was sent"),
                "{}",
                words.text
            );
            assert!(
                words.text.contains("This one can't go through"),
                "{}",
                words.text
            );
            assert!(
                words
                    .text
                    .contains("Hyperliquid explorer before doing anything again"),
                "{}",
                words.text
            );
            assert!(
                !words.text.to_lowercase().contains("nothing new was sent"),
                "{}",
                words.text
            );
            assert!(!words.title.contains("Your"), "{}", words.title);
            assert_eq!(
                words.text.contains("the chain refused it"),
                refused == Some(true),
                "{}",
                words.text
            );
        }
    }

    /// A send stopped because storage couldn't be used is said plainly:
    /// storage is needed to keep track of what the wallet sends, and
    /// nothing was sent.
    #[test]
    fn a_send_stopped_for_want_of_storage_says_storage_is_needed_and_nothing_was_sent() {
        let error = TxError::StorageNeeded;
        assert!(!error.may_have_moved() && !error.after_send());
        let words = error.words(&context());
        assert!(
            words
                .text
                .contains("storage is needed to keep track of what this wallet sends"),
            "{}",
            words.text
        );
        assert!(words.text.contains("nothing was sent"), "{}", words.text);
    }

    #[test]
    fn an_approval_mined_before_the_wallet_left_its_account_is_said_as_that_accounts() {
        for left in [TxError::AccountChanged, TxError::NotConnected] {
            let alone = left.words(&context());
            let words = TxError::ApprovalStands {
                amount: U256::from(1_000_000_000u64),
                block: 101,
                sent: true,
                then: Box::new(left),
            }
            .words(&context());
            assert_eq!(words.title, alone.title);
            assert!(words.text.starts_with(&alone.text), "{}", words.text);
            assert!(
                words.text.ends_with(
                    "The approval of $1,000.00 to the vault from the account that started this went through and stands: depositing $1,000.00 from that account again uses it."
                ),
                "{}",
                words.text
            );
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
    fn a_deposit_refused_when_mined_after_its_approval_says_both_network_fees_were_paid() {
        let error = TxError::ApprovalStands {
            amount: U256::from(1_000_000_000u64),
            block: 101,
            sent: true,
            then: Box::new(TxError::Reverted {
                hash: B256::repeat_byte(7),
                reason: None,
            }),
        };
        let words = error.words(&context());
        assert!(
            !words.text.contains("except the network fee"),
            "{}",
            words.text
        );
        assert!(
            words.text.contains(
                "No money moved. Two network fees were paid: one for the approval and one for the refused deposit."
            ),
            "{}",
            words.text
        );
        assert!(
            words.text.contains("Your approval of $1,000.00"),
            "{}",
            words.text
        );
        // Refused before it was sent: one fee, the approval's.
        let words = TxError::ApprovalStands {
            amount: U256::from(1_000_000_000u64),
            block: 101,
            sent: true,
            then: Box::new(TxError::WouldRevert {
                reason: Revert::Unknown,
            }),
        }
        .words(&context());
        assert!(!words.text.contains("Two network fees"), "{}", words.text);
        assert!(
            words.text.contains("The deposit wasn't sent."),
            "{}",
            words.text
        );
    }

    #[test]
    fn a_deposit_refused_when_mined_on_an_earlier_tries_approval_says_only_its_own_fee() {
        let words = TxError::ApprovalStands {
            amount: U256::from(1_000_000_000u64),
            block: 101,
            sent: false,
            then: Box::new(TxError::Reverted {
                hash: B256::repeat_byte(7),
                reason: None,
            }),
        }
        .words(&context());
        assert!(!words.text.contains("Two network fees"), "{}", words.text);
        assert!(
            words.text.contains("Nothing moved except the network fee."),
            "{}",
            words.text
        );
        assert!(
            words.text.contains("Your approval of $1,000.00"),
            "{}",
            words.text
        );
    }

    #[test]
    fn a_deposit_refused_on_a_node_behind_its_approval_says_the_network_hasnt_caught_up_and_the_approval_stands()
     {
        let words = TxError::ApprovalStands {
            amount: U256::from(1_000_000_000u64),
            block: 101,
            sent: false,
            then: Box::new(TxError::NotCaughtUp),
        }
        .words(&context());
        assert!(
            words
                .text
                .contains("hasn't caught up with your approval yet"),
            "{}",
            words.text
        );
        assert!(
            words.text.contains("Try again in a moment."),
            "{}",
            words.text
        );
        assert!(
            words.text.contains("without asking for it again"),
            "{}",
            words.text
        );
        assert!(!words.text.contains("Nothing was sent."), "{}", words.text);
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
                need: U256::ZERO,
                have: U256::ZERO
            }
            .unsent_words(&context()),
            "your wallet needs a little HYPE on Hyperliquid for its network fee"
        );
        assert_eq!(
            TxError::InsufficientGas {
                need: U256::from(921_800_000_000_000u64),
                have: U256::from(900_000_000_000_000u64)
            }
            .unsent_words(&context()),
            "its network fee needs up to 0.0009218 HYPE on Hyperliquid, and your wallet holds 0.0009 HYPE"
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
