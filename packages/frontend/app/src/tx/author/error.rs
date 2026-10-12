//! What can stop an author's flow, beyond what stops any flow
//! ([`TxError`]), and how each is said: what happened, then what to do.

use alloy_primitives::U256;

use crate::money::format;
use crate::tx::TxError;
use crate::tx::error::{Context, Words};

/// Why an author's flow stopped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthorError {
    /// Anything a depositor's flow can meet too: a rejection, the wrong
    /// network, a revert, the network fee.
    Tx(TxError),
    /// The connected account isn't the agent's author.
    NotAuthor,
    /// The strategy has no review, or isn't registered: nothing to activate.
    NotReady,
    /// It already has a vault.
    AlreadyActive,
    /// The strategy trades on chains besides its vault's, which activation
    /// can't open yet.
    OtherChains,
    /// Under the least a vault may be activated with.
    TankTooSmall { minimum: U256 },
    /// The platform's terms changed since the page read them.
    TermsChanged,
    /// The payment's record on the chain isn't for this upload and account.
    PaymentMismatch,
    /// The idle floor is a share of the vault: at most 100%.
    FloorTooHigh,
    /// No lowering of the idle floor is waiting.
    NothingPending,
    /// The vault isn't open: retiring or a tank fill needs it running.
    NotOpen,
    /// The unsigned withdrawal wasn't asked for, or its wait isn't over.
    EscapeNotReady { ready_at: Option<u64> },
    /// The unsigned withdrawal was asked for too long ago: ask again.
    EscapeLapsed,
    /// The executor serves no signed total for the vault, or not a recent
    /// enough one.
    NoUsageTotal,
}

impl From<TxError> for AuthorError {
    fn from(error: TxError) -> Self {
        Self::Tx(error)
    }
}

impl AuthorError {
    /// Whether a transaction may have been sent, so nothing may say
    /// "Nothing was sent".
    pub fn after_send(&self) -> bool {
        matches!(self, Self::Tx(error) if error.after_send())
    }

    pub fn words(&self, context: &Context<'_>) -> Words {
        let alert = |title: &str, text: String| Words {
            title: title.to_string(),
            text,
            alert: true,
        };
        match self {
            Self::Tx(error) => error.words(context),
            Self::NotAuthor => alert(
                "Only this agent's Author can do that",
                "Connect the wallet that published it. Nothing was sent.".to_string(),
            ),
            Self::NotReady => alert(
                "It isn't ready to activate",
                "Its review and its build aren't both on the chain yet. Nothing was sent."
                    .to_string(),
            ),
            Self::AlreadyActive => alert(
                "It's already active",
                "This agent already has a vault. Nothing was sent.".to_string(),
            ),
            Self::OtherChains => alert(
                "It trades on other chains",
                "Vaults with stations on other chains can't be opened yet. Nothing was sent."
                    .to_string(),
            ),
            Self::TankTooSmall { minimum } => alert(
                "The tank is under the minimum",
                format!(
                    "The starting tank must be at least {}. Nothing was sent.",
                    format::dollars_exact(*minimum, context.decimals)
                ),
            ),
            Self::TermsChanged => alert(
                "The platform's terms changed",
                "They changed after this page read them. Reload the page to read the new terms. Nothing was sent."
                    .to_string(),
            ),
            Self::PaymentMismatch => alert(
                "The payment's record didn't match",
                "The chain recorded a payment, but not for this upload from this wallet. Check your wallet's activity before paying again."
                    .to_string(),
            ),
            Self::FloorTooHigh => alert(
                "That's more than 100%",
                "The idle floor is a share of the vault. Nothing was sent.".to_string(),
            ),
            Self::NothingPending => alert(
                "No change is waiting",
                "The idle floor has no lowering waiting to take effect. Nothing was sent."
                    .to_string(),
            ),
            Self::NotOpen => alert(
                "The vault isn't open",
                "It isn't taking this in its current state. Nothing was sent.".to_string(),
            ),
            Self::EscapeNotReady { ready_at: None } => alert(
                "Ask to withdraw first",
                "Withdrawing without a signed bill needs a request, then a wait. Nothing was sent."
                    .to_string(),
            ),
            Self::EscapeNotReady { ready_at: Some(at) } => alert(
                "Not yet",
                format!(
                    "You can withdraw without a signed bill from {}. Nothing was sent.",
                    utc(*at)
                ),
            ),
            Self::EscapeLapsed => alert(
                "Your request lapsed",
                "Ask to withdraw again, then wait once more. Nothing was sent.".to_string(),
            ),
            Self::NoUsageTotal => alert(
                "No recent bill to settle first",
                "The agent's executor isn't serving a signed bill for this vault right now. You can ask to withdraw without one. Nothing was sent."
                    .to_string(),
            ),
        }
    }
}

/// A chain time as the design writes it: `Sep 30, 17:00 UTC`.
pub fn utc(seconds: u64) -> String {
    let Ok(at) = time::OffsetDateTime::from_unix_timestamp(i64::try_from(seconds).unwrap_or(0))
    else {
        return String::new();
    };
    let this_year = at.year();
    crate::format::day_time(
        time::PrimitiveDateTime::new(at.date(), at.time()),
        this_year,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> Context<'static> {
        Context {
            wallet: "MetaMask",
            chain: "Base",
            wallet_chain: None,
            symbol: "USDC",
            decimals: 6,
            native: "ETH",
        }
    }

    #[test]
    fn the_tank_minimum_is_said_in_exact_dollars() {
        let words = AuthorError::TankTooSmall {
            minimum: U256::from(10_000_000u64),
        }
        .words(&context());
        assert_eq!(
            words.text,
            "The starting tank must be at least $10.00. Nothing was sent."
        );
        assert!(words.alert);
    }

    #[test]
    fn an_error_before_any_send_never_claims_a_transaction() {
        assert!(!AuthorError::NotAuthor.after_send());
        assert!(
            AuthorError::Tx(TxError::Dropped {
                hash: alloy_primitives::B256::ZERO
            })
            .after_send()
        );
    }
}
