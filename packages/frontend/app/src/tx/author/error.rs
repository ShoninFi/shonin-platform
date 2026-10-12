//! What can stop an author's flow, beyond what stops any flow
//! ([`TxError`]), and how each is said: what happened, then what to do.

use alloy_primitives::{Address, B256, U256};

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
    /// The connected account isn't the one the upload named: only that
    /// wallet's payment starts the job, so nothing is sent from another.
    /// `approved` is the fee's approval when it was mined before the payment
    /// was stopped: it stands, and the uploader's payment uses it.
    WrongPayer {
        uploader: Address,
        approved: Option<U256>,
    },
    /// The payment was sent and wasn't seen mined (dropped, stuck or
    /// replaced): it may still land. `since` is how many payments the
    /// publishing contract had recorded before it was sent, so the chain
    /// can be searched for it.
    PaymentUnsure {
        upload: B256,
        since: U256,
        then: Box<TxError>,
    },
    /// The activation stopped after its approval of the tank (`amount`) was
    /// mined: the approval stands; trying again with the same tank uses it,
    /// and with another replaces it with exactly that tank.
    ApprovalStands {
        spender: super::flow::Spender,
        amount: U256,
        then: Box<AuthorError>,
    },
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
    /// The bill so far, settled from the tank first, would leave nothing to
    /// withdraw.
    TabTakesTank { tab: U256 },
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
        match self {
            Self::Tx(error) => error.after_send(),
            Self::PaymentMismatch | Self::PaymentUnsure { .. } | Self::ApprovalStands { .. } => {
                true
            }
            Self::WrongPayer { approved, .. } => approved.is_some(),
            _ => false,
        }
    }

    /// Whether a payment for the upload may have been made (or may still
    /// land): the page must not offer to pay again until the chain shows
    /// no payment for it.
    pub fn may_have_paid(&self) -> bool {
        matches!(self, Self::PaymentMismatch | Self::PaymentUnsure { .. })
            || matches!(self, Self::Tx(error) if error.may_have_moved())
    }

    /// The transaction the error is about, for its link.
    pub fn hash(&self) -> Option<B256> {
        match self {
            Self::Tx(error) => error.hash(),
            Self::PaymentUnsure { then, .. } => then.hash(),
            Self::ApprovalStands { then, .. } => then.hash(),
            _ => None,
        }
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
            Self::WrongPayer { uploader, approved } => {
                let uploader = crate::format::short_hash(&format!("{uploader:?}"));
                let sent = match approved {
                    None => "Nothing was sent.".to_string(),
                    Some(fee) => format!(
                        "The payment wasn't sent. Its approval of {} {} went through and stands: {uploader}'s payment uses it, without asking for it again.",
                        crate::money::format_exact(*fee, context.decimals, 0),
                        context.symbol
                    ),
                };
                alert(
                    "Switch back to the wallet that made this upload",
                    format!(
                        "Switch back to {uploader}, which made this upload: only its payment starts the job. {sent} To pay from this wallet instead, replace the file and upload it again."
                    ),
                )
            }
            Self::PaymentUnsure { then, .. } => {
                let wallet = context.wallet;
                let seen = match then.as_ref() {
                    TxError::Dropped { .. } => {
                        "The network this app reads hasn't seen your payment.".to_string()
                    }
                    TxError::Stuck { .. } => {
                        "Your payment was sent and still isn't mined after a long wait.".to_string()
                    }
                    TxError::Replaced { .. } => format!(
                        "Another transaction took your payment's place in {wallet}: it may have been sped up or cancelled."
                    ),
                    _ => "Your payment was sent and hasn't been seen mined.".to_string(),
                };
                alert(
                    "Your payment may still go through",
                    format!(
                        "{seen} It may still land. Check {wallet}'s activity, then check the chain for it here before paying again."
                    ),
                )
            }
            Self::ApprovalStands {
                spender,
                amount,
                then,
            } => {
                use super::flow::Spender;
                let (step, title, to, again) = match spender {
                    Spender::Factory => (
                        "The activation wasn't sent.",
                        "Not activated: your approval stands",
                        "the vault factory",
                        "activating again with a {amount} tank uses it, without asking for it again; another tank replaces it with an approval of exactly that tank.",
                    ),
                    Spender::Vault => (
                        "The top-up wasn't sent.",
                        "Not topped up: your approval stands",
                        "the vault",
                        "topping up {amount} again uses it, without asking for it again; another amount replaces it with an approval of exactly that amount.",
                    ),
                    Spender::Publishing => (
                        "The payment wasn't sent.",
                        "Not paid: your approval stands",
                        "the publishing contract",
                        "paying again uses it, without asking for it again.",
                    ),
                };
                let words = then.words(context);
                // The inner words say nothing moved; the approval did.
                let reason = ["Nothing was sent.", "No money moved and no request was made."]
                    .iter()
                    .find_map(|said| words.text.strip_suffix(said))
                    .map(|text| format!("{text}{step}"))
                    .unwrap_or(words.text);
                // The fee is said in the dollar's own name, as the publish
                // page says it.
                let amount = match spender {
                    Spender::Publishing => format!(
                        "{} {}",
                        crate::money::format_exact(*amount, context.decimals, 0),
                        context.symbol
                    ),
                    _ => format::dollars_exact(*amount, context.decimals),
                };
                let again = again.replace("{amount}", &amount);
                // Its own title: the approval went, so no title of a step
                // that sent nothing stands above it.
                alert(
                    title,
                    format!(
                        "{reason} Your approval of {amount} to {to} went through and stands: {again}"
                    ),
                )
            }
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
            Self::TabTakesTank { tab } => alert(
                "The tab takes what's in the tank",
                format!(
                    "The bill so far, up to {}, is paid from the tank first and leaves nothing to withdraw. Nothing was sent.",
                    format::dollars_exact(*tab, context.decimals)
                ),
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
