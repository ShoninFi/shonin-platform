//! Everything that can stop a Solana flow, and how each is said: in words,
//! in the place the action is (say what happened, then what to do), with the
//! same names and tone as the EVM flows' ([`crate::tx::error::Words`]).

use alloy_primitives::U256;

use super::port::{Failure, Sig};
use crate::money::{Rounding, format};
use crate::tx::error::{Context, Words};

/// Why a Solana flow stopped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SolError {
    /// The person declined in their wallet.
    Rejected,
    /// Nothing is connected.
    NotConnected,
    /// The wallet's account changed since the flow began.
    AccountChanged,
    /// The person cancelled before anything was sent.
    Cancelled,
    /// The wallet doesn't offer this cluster (`solana:devnet`).
    WrongCluster,
    /// The endpoint answers for another cluster than the vault's.
    WrongEndpoint,
    /// The vault's accounts aren't the program's this app was built for.
    WrongAccount,
    /// More than the wallet holds of the dollar.
    InsufficientBalance { have: u64 },
    /// More shares than the wallet holds.
    InsufficientShares { have: u128 },
    /// Not enough SOL for the network fee and the accounts' rent.
    InsufficientSol,
    /// Under the vault's minimum deposit.
    BelowMinimum { minimum: u64 },
    /// Nothing typed, or zero.
    NothingToSend,
    /// The vault isn't in a state that takes this.
    WrongState,
    /// The vault's restricted list isn't the one shown: nothing is signed.
    ListChanged,
    /// The cluster would refuse it: nothing was sent.
    WouldFail { reason: Reason },
    /// Landed, and failed: nothing moved but the network fee.
    Failed {
        signature: Sig,
        reason: Option<Reason>,
    },
    /// Its blockhash expired before it landed: it can never land now.
    Expired { signature: Sig },
    /// Sent, and not seen landed after a long wait.
    Stuck { signature: Sig },
    /// The request landed and stands, but what became of it after couldn't
    /// be followed: never "nothing was sent".
    Pending { signature: Sig, why: String },
    /// The wallet changed the transaction before signing it: not sent.
    Altered,
    /// The wallet's signature isn't the account's over the transaction.
    BadSignature,
    /// The transaction is larger than the network takes.
    TooLarge,
    /// The cluster or the wallet answered with something unexpected.
    Network(String),
    /// Anything else the wallet said.
    Wallet(String),
}

/// What the cluster said, as far as it can be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reason {
    /// One of the vault program's own errors, by name.
    Vault(&'static str),
    /// The dollar's token program refused: more than the account holds.
    TokenBalance,
    /// Not enough SOL for the fee or an account's rent.
    Lamports,
    /// A message from the logs.
    Message(String),
    Unknown,
}

impl Reason {
    /// Read a failure: the vault's error from the logs Anchor writes, or its
    /// number in the error; else the token program's or the runtime's.
    pub fn from_failure(failure: &Failure) -> Self {
        for line in &failure.logs {
            if let Some(rest) = line.split("Error Code: ").nth(1) {
                let name = rest.split('.').next().unwrap_or_default();
                if let Some(known) = (6000..6100)
                    .filter_map(chain::solana_ix::accounts::vault_error)
                    .find(|known| *known == name)
                {
                    return Self::Vault(known);
                }
            }
        }
        let logs = failure.logs.join("\n").to_ascii_lowercase();
        if logs.contains("insufficient lamports")
            || failure.err.contains("InsufficientFundsForFee")
            || failure.err.contains("InsufficientFundsForRent")
            || failure.err.contains("AccountNotFound")
        {
            return Self::Lamports;
        }
        if logs.contains("error: insufficient funds") {
            return Self::TokenBalance;
        }
        if let Some(code) = custom_code(&failure.err)
            && let Some(name) = chain::solana_ix::accounts::vault_error(code)
        {
            return Self::Vault(name);
        }
        match failure
            .logs
            .iter()
            .rev()
            .find_map(|line| line.split("Error Message: ").nth(1))
        {
            Some(message) => Self::Message(message.trim_end_matches('.').to_string()),
            None => Self::Unknown,
        }
    }

    pub fn words(&self) -> String {
        match self {
            Self::Vault("WrongState") => {
                "The vault isn't taking this right now: it may have closed or stopped.".to_string()
            }
            Self::Vault("TooSmall") => "That's under the vault's minimum.".to_string(),
            Self::Vault("NoStatement") => {
                "The vault's restricted list changed. Read it again before you deposit.".to_string()
            }
            Self::Vault("NotAllowed") => {
                "Only the owner can do that, or it isn't open to anyone yet.".to_string()
            }
            Self::Vault("NotYet") => "It isn't time for that yet.".to_string(),
            Self::Vault("Nothing") => "There's nothing to collect.".to_string(),
            Self::Vault("Unsettled") => {
                "The vault is settling a valuation. Try again in a few seconds.".to_string()
            }
            Self::Vault("BelowFloor") => {
                "That would take the vault under its idle floor.".to_string()
            }
            Self::Vault(name) => format!("The vault refused it ({name})."),
            Self::TokenBalance => "That's more than the wallet holds.".to_string(),
            Self::Lamports => "The wallet needs a little more SOL for the network fee.".to_string(),
            Self::Message(message) => format!("The network refused it: {message}."),
            Self::Unknown => "The network refused it.".to_string(),
        }
    }
}

/// The number in a node's `{"Custom":6000}` error.
fn custom_code(err: &str) -> Option<u32> {
    let rest = err.split("\"Custom\":").nth(1)?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

impl SolError {
    /// Whether a transaction may have been sent, so the page must not say
    /// "Nothing was sent".
    pub fn after_send(&self) -> bool {
        matches!(
            self,
            Self::Failed { .. } | Self::Stuck { .. } | Self::Pending { .. }
        )
    }

    /// The transaction the error is about, for its link.
    pub fn signature(&self) -> Option<Sig> {
        match self {
            Self::Failed { signature, .. }
            | Self::Expired { signature }
            | Self::Stuck { signature }
            | Self::Pending { signature, .. } => Some(*signature),
            _ => None,
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
            Self::NotConnected => alert(
                "No Solana wallet connected",
                "Connect a Solana wallet to continue. Nothing was sent.".to_string(),
            ),
            Self::AccountChanged => alert(
                "Your wallet's account changed",
                "This was set up for another account, so it stopped. Nothing was sent from the new one."
                    .to_string(),
            ),
            Self::WrongCluster => alert(
                "Wrong network",
                format!(
                    "{wallet} doesn't offer {} for this account. Switch its network to {} and try again. Nothing was sent.",
                    context.chain, context.chain
                ),
            ),
            Self::WrongEndpoint => alert(
                "The network didn't answer as expected",
                "The Solana endpoint answered for another cluster. Nothing was sent.".to_string(),
            ),
            Self::WrongAccount => alert(
                "This vault doesn't read as expected",
                "Its accounts aren't the ones this app was built for. Nothing was sent.".to_string(),
            ),
            Self::InsufficientBalance { have } => alert(
                "More than your wallet holds",
                format!(
                    "You have {} on {}.",
                    format::token_balance(U256::from(*have), context.decimals, context.symbol),
                    context.chain
                ),
            ),
            Self::InsufficientShares { have } => alert(
                "More shares than you hold",
                format!(
                    "You hold {} shares.",
                    format::shares(U256::from(*have), Rounding::Down)
                ),
            ),
            Self::InsufficientSol => alert(
                "Not enough for the network fee",
                format!(
                    "Your wallet needs a little SOL on {} to pay the network fee and the request's deposit. Nothing was sent.",
                    context.chain
                ),
            ),
            Self::BelowMinimum { minimum } => alert(
                "Below the minimum",
                format!(
                    "The smallest deposit is {}.",
                    format::dollars_exact(U256::from(*minimum), context.decimals).replace(".00", "")
                ),
            ),
            Self::NothingToSend => alert("Type an amount", "Nothing was sent.".to_string()),
            Self::WrongState => alert(
                "Not possible right now",
                "The vault isn't taking this in its current state. Nothing was sent.".to_string(),
            ),
            Self::ListChanged => alert(
                "The vault's restricted list changed",
                "It isn't the list you were shown, so the app stopped before asking you to sign. Read it again and confirm. Nothing was signed or sent."
                    .to_string(),
            ),
            Self::WouldFail { reason } => alert(
                "The network wouldn't accept it. Nothing moved.",
                format!("{} Nothing was sent.", reason.words()),
            ),
            Self::Failed { reason, .. } => alert(
                "Transaction failed",
                format!(
                    "{} Nothing moved except the network fee.",
                    reason
                        .as_ref()
                        .map(Reason::words)
                        .unwrap_or_else(|| "The network didn't accept it.".to_string())
                ),
            ),
            Self::Expired { .. } => alert(
                "It expired before the network took it",
                "Nothing moved, and it can't go through now. Try again to send a new one: your wallet asks you again."
                    .to_string(),
            ),
            Self::Stuck { .. } => alert(
                "Still waiting for the network",
                format!(
                    "The network hasn't confirmed your transaction yet. It may still go through: check {wallet} before trying again."
                ),
            ),
            Self::Pending { why, .. } => alert(
                "Your request was sent and stands",
                format!(
                    "It landed on the network, but the app couldn't follow it to the end: {why}. Don't send it again: your portfolio shows it once it's settled."
                ),
            ),
            Self::Altered => alert(
                "Your wallet changed the transaction",
                format!("{wallet} returned a different transaction from the one shown, so the app didn't send it. Nothing was sent."),
            ),
            Self::BadSignature => alert(
                "The signature didn't match",
                format!("{wallet} signed with another account or other content. Nothing was sent."),
            ),
            Self::TooLarge => alert(
                "Too large to send",
                "This transaction is larger than the network takes. Nothing was sent.".to_string(),
            ),
            Self::Network(why) => alert(
                "The network didn't answer",
                format!("{why}. Check your connection and try again."),
            ),
            Self::Wallet(why) => alert("Your wallet refused it", format!("{wallet} said: {why}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failure(err: &str, logs: &[&str]) -> Failure {
        Failure {
            err: err.to_string(),
            logs: logs.iter().map(|line| line.to_string()).collect(),
        }
    }

    #[test]
    fn a_vault_error_is_read_from_anchors_log_line_or_its_number() {
        let logged = failure(
            r#"{"InstructionError":[2,{"Custom":6003}]}"#,
            &[
                "Program log: AnchorError occurred. Error Code: NoStatement. Error Number: 6003. Error Message: The depositor has no statement.",
            ],
        );
        assert_eq!(Reason::from_failure(&logged), Reason::Vault("NoStatement"));
        let numbered = failure(r#"{"InstructionError":[2,{"Custom":6000}]}"#, &[]);
        assert_eq!(Reason::from_failure(&numbered), Reason::Vault("WrongState"));
    }

    #[test]
    fn running_out_of_sol_and_of_dollars_are_told_apart() {
        assert_eq!(
            Reason::from_failure(&failure(r#""AccountNotFound""#, &[])),
            Reason::Lamports
        );
        assert_eq!(
            Reason::from_failure(&failure(
                r#"{"InstructionError":[1,{"Custom":0}]}"#,
                &["Transfer: insufficient lamports 100, need 2000"]
            )),
            Reason::Lamports
        );
        assert_eq!(
            Reason::from_failure(&failure(
                r#"{"InstructionError":[2,{"Custom":1}]}"#,
                &["Program log: Error: insufficient funds"]
            )),
            Reason::TokenBalance
        );
    }

    #[test]
    fn a_rejection_says_nothing_was_sent_and_is_not_an_alert() {
        let context = Context {
            wallet: "Phantom",
            chain: "Solana devnet",
            wallet_chain: None,
            symbol: "USDC",
            decimals: 6,
            native: "SOL",
        };
        let words = SolError::Rejected.words(&context);
        assert_eq!(words.title, "Rejected in wallet. Nothing was sent.");
        assert!(!words.alert);
        let words = SolError::InsufficientBalance { have: 812_409_999 }.words(&context);
        assert_eq!(words.text, "You have 812.40 USDC on Solana devnet.");
        let words = SolError::Expired {
            signature: Sig([1; 64]),
        }
        .words(&context);
        assert!(words.text.contains("Nothing moved"));
    }
}
