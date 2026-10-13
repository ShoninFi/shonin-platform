//! What the author's pages share: a flow's status line (one live region
//! whose words change, with errors said in place), what the wallet is asked
//! and what came of it in words, labelled fields, the wallet gate in front
//! of an action, and small formatting rules.

use std::rc::Rc;

use alloy_primitives::{B256, U256};
use futures_signals::signal::{Mutable, Signal, SignalExt};
use ui::icons::Icon;

use crate::atoms::button::{Button, Kind, Size, on_click};
use crate::atoms::icon;
use crate::components::wallet::{hex_ring, open_connect, switch_button};
use crate::money::Rounding;
use crate::money::format as money;
use crate::tx::author::error::utc;
use crate::tx::author::form::percent;
use crate::tx::author::run::Running;
use crate::tx::author::{AuthorError, Outcome, Prompt, Spender, Step};
use crate::tx::error::Context;
use crate::view::{El, View, el, text};
use crate::wallet::machine::Connection;
use crate::wallet::{Network, networks};

/// The names a flow's words need: the wallet, the chain, the dollar.
#[derive(Clone, Debug)]
pub struct Names {
    pub wallet: String,
    pub chain: String,
    pub chain_id: u64,
    pub symbol: String,
    pub decimals: u8,
    pub native: String,
}

impl Names {
    pub fn of(network: &Network) -> Self {
        let wallet = crate::wallet::get().wallet_name.get_cloned();
        Self {
            wallet: if wallet.is_empty() {
                "your wallet".to_string()
            } else {
                wallet
            },
            chain: network.label.clone(),
            chain_id: network.chain_id,
            symbol: network.asset_symbol.clone(),
            decimals: network.asset_decimals,
            native: network.coin().to_string(),
        }
    }

    pub fn dollars(&self, value: U256) -> String {
        money::dollars_exact(value, self.decimals)
    }

    /// An amount of the dollar named by its symbol, as the publishing fee
    /// is written: `1 USDC`, `1.5 USDC`, every digit it has.
    pub fn tokens(&self, value: U256) -> String {
        let amount = crate::money::format_exact(value, self.decimals, 0);
        format!("{amount} {}", self.symbol)
    }

    /// An error in words, with the chain the wallet is on now.
    pub fn words(&self, error: &AuthorError) -> crate::tx::error::Words {
        let on = crate::wallet::get()
            .wallet_chain()
            .map(networks::chain_name);
        let words = error.words(&Context {
            wallet: &self.wallet,
            chain: &self.chain,
            wallet_chain: on.as_deref(),
            symbol: &self.symbol,
            decimals: self.decimals,
            native: &self.native,
        });
        // A line may start with the wallet's name: each reads as a sentence.
        crate::tx::error::Words {
            title: crate::format::sentence(&words.title),
            text: crate::format::sentence(&words.text),
            alert: words.alert,
        }
    }
}

fn spender(who: Spender) -> &'static str {
    match who {
        Spender::Publishing => "the publishing contract",
        Spender::Factory => "the vault factory",
        Spender::Vault => "the vault",
    }
}

/// What the wallet is being asked, in words: a title and a line.
pub fn prompt_words(prompt: &Prompt, names: &Names) -> (String, String) {
    let wallet = &names.wallet;
    match prompt {
        Prompt::Statement => (
            format!("Sign the statement in {wallet}"),
            "You say you aren't a restricted person for this vault, as every holder does. It's a signature, not a transaction.".to_string(),
        ),
        Prompt::Approve {
            amount,
            spender: Spender::Publishing,
        } => (
            format!("Approve {} in {wallet}", names.tokens(*amount)),
            format!(
                "This lets {} take exactly {} of yours, and no more.",
                spender(Spender::Publishing),
                names.tokens(*amount)
            ),
        ),
        Prompt::Approve { amount, spender: who } => (
            format!("Approve {} in {wallet}", names.dollars(*amount)),
            format!(
                "This lets {} take exactly {} of your {}, and no more.",
                spender(*who),
                names.dollars(*amount),
                names.symbol
            ),
        ),
        Prompt::Pay { fee } => (
            format!("Pay {} in {wallet}", names.tokens(*fee)),
            "The fee for the review and the build, paid against your upload.".to_string(),
        ),
        Prompt::Activate { tank } => (
            format!("Activate in {wallet}"),
            format!(
                "One transaction opens the vault and moves {} into its tank.",
                names.dollars(*tank)
            ),
        ),
        Prompt::FundTank { amount } => (
            format!("Top up in {wallet}"),
            format!("{} goes into the tank.", names.dollars(*amount)),
        ),
        Prompt::SetKeep { amount } if amount.is_zero() => (
            format!("Turn auto-fill off in {wallet}"),
            "The tank stops topping itself up from your shares.".to_string(),
        ),
        Prompt::SetKeep { amount } => (
            format!("Turn auto-fill on in {wallet}"),
            format!(
                "Once a day the tank is topped back up to {} from your shares.",
                names.dollars(*amount)
            ),
        ),
        Prompt::TopUpFromShares { shares } => (
            format!("Turn shares into tank money in {wallet}"),
            format!(
                "{} shares are priced at the next valuation and their dollars go into the tank.",
                money::shares(*shares, Rounding::Down)
            ),
        ),
        Prompt::SetAllowances => (
            format!("Save the allowances in {wallet}"),
            "They take effect at once and the vault's page shows the change.".to_string(),
        ),
        Prompt::SetIdleFloor { bps } => (
            format!("Change the idle floor in {wallet}"),
            format!("To {}.", percent(*bps)),
        ),
        Prompt::CancelFloorChange { bps } => (
            format!("Cancel the change in {wallet}"),
            format!("The idle floor stays at {}.", percent(*bps)),
        ),
        Prompt::Retire => (
            format!("Retire in {wallet}"),
            "Deposits stop as soon as it's mined.".to_string(),
        ),
        Prompt::RequestEscape => (
            format!("Ask to withdraw in {wallet}"),
            "The wait before you can withdraw without a signed bill starts now.".to_string(),
        ),
        Prompt::WithdrawTank { amount, tab } if tab.is_zero() => (
            format!("Withdraw {} in {wallet}", names.dollars(*amount)),
            format!(
                "{} goes from the tank to your wallet.",
                names.dollars(*amount)
            ),
        ),
        Prompt::WithdrawTank { amount, tab } => (
            format!("Withdraw {} in {wallet}", names.dollars(*amount)),
            format!(
                "The bill so far, up to {}, is paid from the tank first; then {} goes to your wallet.",
                names.dollars(*tab),
                names.dollars(*amount)
            ),
        ),
    }
}

/// How a flow ended well, in words: a title and a line.
pub fn outcome_words(outcome: &Outcome, names: &Names) -> (String, String) {
    match outcome {
        Outcome::Paid { strategy_id, .. } => (
            "Paid from the wallet that uploaded it".to_string(),
            format!(
                "Your agent's id is {strategy_id}. The job starts once the build service has seen the payment confirmed on the chain."
            ),
        ),
        Outcome::AlreadyPaid { strategy_id } => (
            "Already paid from the wallet that uploaded it".to_string(),
            format!(
                "The chain already records this upload's payment from this wallet, as agent {strategy_id}, so nothing was sent. The job starts once the build service has seen it confirmed on the chain."
            ),
        ),
        Outcome::Activated { .. } => (
            "Your vault is open".to_string(),
            "It's listed under New agents until it has 30 days of history.".to_string(),
        ),
        Outcome::TankFunded { amount, .. } => (
            "Tank topped up".to_string(),
            format!("{} went into the tank.", names.dollars(*amount)),
        ),
        Outcome::KeepSet { amount, .. } if amount.is_zero() => (
            "Auto-fill is off".to_string(),
            "The tank no longer tops itself up.".to_string(),
        ),
        Outcome::KeepSet { amount, .. } => (
            "Auto-fill is on".to_string(),
            format!(
                "The tank is kept at {}; the first fill comes with the next daily collection.",
                names.dollars(*amount)
            ),
        ),
        Outcome::FillQueued { shares, .. } => (
            "Queued for the next valuation".to_string(),
            format!(
                "{} shares turn into tank money when it prices them.",
                money::shares(*shares, Rounding::Down)
            ),
        ),
        Outcome::AllowancesSet { .. } => (
            "Allowances saved".to_string(),
            "They apply from now on.".to_string(),
        ),
        Outcome::FloorSet {
            bps, effective_at, ..
        } => (
            format!("Idle floor set to {}", percent(*bps)),
            format!("It takes effect {}.", utc(*effective_at)),
        ),
        Outcome::FloorKept { bps, .. } => (
            "Change cancelled".to_string(),
            format!("The idle floor stays at {}.", percent(*bps)),
        ),
        Outcome::Retired { .. } => (
            "Retired".to_string(),
            "Deposits have stopped. Your agent keeps running for the wind-down.".to_string(),
        ),
        Outcome::EscapeRequested { ready_at, .. } => (
            "Asked".to_string(),
            format!(
                "You can withdraw without a signed bill from {}.",
                utc(*ready_at)
            ),
        ),
        Outcome::TankWithdrawn { amount, .. } => (
            "Sent to your wallet".to_string(),
            format!("{} came out of the tank.", names.dollars(*amount)),
        ),
    }
}

fn outcome_hash(outcome: &Outcome) -> Option<B256> {
    let hash = match outcome {
        Outcome::AlreadyPaid { .. } => return None,
        Outcome::Paid { hash, .. }
        | Outcome::Activated { hash, .. }
        | Outcome::TankFunded { hash, .. }
        | Outcome::KeepSet { hash, .. }
        | Outcome::FillQueued { hash, .. }
        | Outcome::AllowancesSet { hash }
        | Outcome::FloorSet { hash, .. }
        | Outcome::FloorKept { hash, .. }
        | Outcome::Retired { hash }
        | Outcome::EscapeRequested { hash, .. }
        | Outcome::TankWithdrawn { hash, .. } => *hash,
    };
    Some(hash)
}

/// A transaction, linked to the chain's explorer where there is one.
pub fn tx_link(chain_id: u64, hash: B256, label: &'static str) -> View {
    let full = format!("{hash:?}");
    match networks::tx_url(chain_id, hash) {
        Some(url) => el("a")
            .class("sh-author__txlink")
            .attr("href", url)
            .attr("target", "_blank")
            .attr("rel", "noopener noreferrer")
            .child(el("span").class("sh-label").text(label))
            .child(
                el("span")
                    .class("sh-mono")
                    .text(crate::format::short_hash(&full)),
            )
            .child(icon(Icon::External, 14))
            .child(
                el("span")
                    .class("sh-vh")
                    .text(format!("{full} (opens the explorer)")),
            )
            .into(),
        None => crate::atoms::hash::hash(&full, Some(label), "transaction hash").into(),
    }
}

/// The words a step announces, once, in the live region.
fn announce(step: &Option<Step>, names: &Names) -> String {
    match step {
        None => String::new(),
        Some(Step::Checking) => "Checking the chain".to_string(),
        Some(Step::Signing(prompt)) => prompt_words(prompt, names).0,
        Some(Step::Sent { .. }) => "Sent. Waiting for the network to confirm it.".to_string(),
        Some(Step::Done(outcome)) => outcome_words(outcome, names).0,
        // An error is said by its own alert or status in the body.
        Some(Step::Failed(_)) => String::new(),
    }
}

/// A flow's status in place: the step drawn in the body, and one polite
/// live region whose words change with it (never removed and re-added).
pub fn status(running: &Running, names: Rc<Names>) -> El {
    let live_names = names.clone();
    let cancel = running.cancel.clone();
    el("div")
        .class("sh-author__status")
        .child(
            el("p")
                .class("sh-vh")
                .attr("aria-live", "polite")
                .text_signal(
                    "",
                    running
                        .step
                        .signal_cloned()
                        .map(move |step| announce(&step, &live_names)),
                ),
        )
        .child_signal(
            None,
            running.step.signal_cloned().map(move |step| {
                let step = step?;
                Some(step_view(&step, &names, &cancel))
            }),
        )
}

fn step_view(step: &Step, names: &Names, cancel: &crate::tx::flow::Cancel) -> View {
    match step {
        Step::Checking => el("div")
            .class("sh-author__wait")
            .child(hex_ring())
            .child(
                el("p")
                    .class("sh-author__steptitle")
                    .text("Checking the chain"),
            )
            .child(
                el("p")
                    .class("sh-muted")
                    .text(format!("Reading what this needs on {}.", names.chain)),
            )
            .into(),
        Step::Signing(prompt) => {
            let (title, line) = prompt_words(prompt, names);
            let cancel = cancel.clone();
            el("div")
                .class("sh-author__wait")
                .child(hex_ring())
                .child(el("p").class("sh-author__steptitle").text(title))
                .child(el("p").text(line))
                .child(el("p").class("sh-muted sh-author__small").text(format!(
                    "Don’t see it? Open {}. Nothing is sent until you approve.",
                    names.wallet
                )))
                .child(on_click(
                    Button::new(Kind::Secondary, "Cancel")
                        .size(Size::Small)
                        .build(),
                    move || cancel.cancel(),
                ))
                .into()
        }
        Step::Sent { hash, .. } => el("div")
            .class("sh-author__wait")
            .child(hex_ring())
            .child(el("p").class("sh-author__steptitle").text("Sent"))
            .child(el("p").text("Waiting for the network to confirm it."))
            .child(tx_link(names.chain_id, *hash, "Transaction"))
            .into(),
        Step::Done(outcome) => {
            let (title, line) = outcome_words(outcome, names);
            el("div")
                .class("sh-author__done")
                .attr("role", "status")
                .child(
                    el("p")
                        .class("sh-author__steptitle")
                        .child(icon(Icon::Check, 18))
                        .text(title),
                )
                .child(el("p").text(line))
                .child(
                    outcome_hash(outcome)
                        .map(|hash| tx_link(names.chain_id, hash, "Transaction"))
                        .unwrap_or(View::Empty),
                )
                .into()
        }
        Step::Failed(error) => failed(error, names),
    }
}

/// How a flow failed, in words, in place: an alert where it needs acting
/// on, else a status; the transaction, if one was sent.
pub fn failed(error: &AuthorError, names: &Names) -> View {
    let words = names.words(error);
    let hash = error.hash();
    el("div")
        .class("sh-author__error")
        .class_if(words.alert, "sh-author__error--alert")
        .attr("role", if words.alert { "alert" } else { "status" })
        .child(el("p").class("sh-author__steptitle").text(words.title))
        .child(el("p").text(words.text))
        .child(match hash {
            Some(hash) => tx_link(names.chain_id, hash, "Transaction"),
            None => View::Empty,
        })
        .into()
}

/// A labelled text field: `inputmode` decimal, its hint or error under it
/// and linked to it, the unit before or after.
pub fn field(
    id: &'static str,
    label: &str,
    before: Option<&'static str>,
    after: Option<String>,
    value: &Mutable<String>,
    hint: String,
    error: impl Signal<Item = Option<String>> + 'static,
) -> El {
    let typed = value.clone();
    let error = error.broadcast();
    let hint_id = format!("{id}-hint");
    let shown_hint = hint.clone();
    el("div")
        .class("sh-field")
        .class_signal("sh-field--error", false, error.signal_ref(Option::is_some))
        .child(
            el("label")
                .class("sh-field__label")
                .attr("for", id)
                .text(label.to_string()),
        )
        .child(
            el("div")
                .class("sh-input")
                .child(match before {
                    Some(unit) => el("span")
                        .class("sh-input__unit")
                        .attr("aria-hidden", "true")
                        .text(unit)
                        .into(),
                    None => View::Empty,
                })
                .child(
                    el("input")
                        .attr("id", id)
                        .attr("type", "text")
                        .attr("inputmode", "decimal")
                        .attr("autocomplete", "off")
                        .attr("spellcheck", "false")
                        .attr("aria-describedby", hint_id.clone())
                        .attr_signal(
                            "aria-invalid",
                            None,
                            error.signal_ref(|error| error.as_ref().map(|_| "true".to_string())),
                        )
                        .attr("value", value.get_cloned())
                        .hook(move |dom| {
                            dominator::with_node!(dom, input => {
                                .prop_signal("value", typed.signal_cloned())
                                .event(move |_: dominator::events::Input| {
                                    if let Some(input) = wasm_bindgen::JsCast::dyn_ref::<web_sys::HtmlInputElement>(&input) {
                                        typed.set_neq(input.value());
                                    }
                                })
                            })
                        }),
                )
                .child(match after {
                    Some(unit) => el("span").class("sh-input__unit").text(unit).into(),
                    None => View::Empty,
                }),
        )
        .child(
            el("p")
                .class("sh-field__hint")
                .attr("id", hint_id)
                .text_signal(
                    hint,
                    error.signal_cloned().map(move |error| error.unwrap_or_else(|| shown_hint.clone())),
                ),
        )
}

/// What stands in front of an action that signs: "Connect wallet" while
/// signed out, "Switch to <chain>" while a browser wallet is on another
/// chain, else the action itself.
pub fn wallet_gate(chain_id: u64, action: impl Fn() -> View + 'static) -> View {
    let session = crate::wallet::get();
    el("div")
        .class("sh-author__gate")
        // The header's chip compares the wallet's chain with this one.
        .hook(move |dom| {
            dom.future(crate::components::wallet::acting_on(
                futures_signals::signal::always(Some(chain_id)),
            ))
        })
        .child_signal(
            None,
            session.connection.signal_cloned().map(move |connection| {
                Some(match connection {
                    Connection::Connected {
                        chain_id: Some(on), ..
                    } if on != chain_id => switch_button(chain_id, Size::Large)
                        .class("sh-btn-block")
                        .into(),
                    Connection::Connected { .. } => action(),
                    _ => on_click(
                        Button::new(Kind::Primary, "Connect wallet")
                            .size(Size::Large)
                            .block()
                            .build(),
                        open_connect,
                    )
                    .attr("aria-haspopup", "dialog")
                    .into(),
                })
            }),
        )
        .into()
}

/// `1st`, `2nd`, `3rd`, `11th`, `22nd`.
pub fn ordinal(n: u32) -> String {
    let suffix = match (n % 10, n % 100) {
        (_, 11..=13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    };
    format!("{n}{suffix}")
}

/// A short labelled row of a summary list.
pub fn row(label: &str, value: impl Into<View>) -> View {
    View::Many(vec![
        el("dt").text(label.to_string()).into(),
        el("dd").child(value.into()).into(),
    ])
}

/// A summary list's row whose value follows a signal.
pub fn row_signal(label: &str, now: String, value: impl Signal<Item = String> + 'static) -> View {
    View::Many(vec![
        el("dt").text(label.to_string()).into(),
        el("dd").text_signal(now, value).into(),
    ])
}

/// Text as it is, for when a view takes a `View`.
pub fn words(value: impl Into<String>) -> View {
    text(value.into())
}

/// The reader's year, from the app's clock (the browser has no system clock
/// for `time`); `otherwise` before the clock has started.
pub fn this_year(otherwise: i32) -> i32 {
    let millis = crate::clock::now().get();
    if millis <= 0.0 {
        return otherwise;
    }
    time::OffsetDateTime::from_unix_timestamp((millis / 1_000.0) as i64)
        .map(|now| now.year())
        .unwrap_or(otherwise)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queue_places_read_as_ordinals() {
        assert_eq!(ordinal(1), "1st");
        assert_eq!(ordinal(2), "2nd");
        assert_eq!(ordinal(3), "3rd");
        assert_eq!(ordinal(4), "4th");
        assert_eq!(ordinal(11), "11th");
        assert_eq!(ordinal(12), "12th");
        assert_eq!(ordinal(13), "13th");
        assert_eq!(ordinal(21), "21st");
        assert_eq!(ordinal(102), "102nd");
        assert_eq!(ordinal(111), "111th");
    }

    #[test]
    fn an_approval_says_exactly_what_it_lets_be_taken_and_the_fee_is_named_in_usdc() {
        let names = Names {
            wallet: "MetaMask".to_string(),
            chain: "Base".to_string(),
            chain_id: 84_532,
            symbol: "USDC".to_string(),
            decimals: 6,
            native: "ETH".to_string(),
        };
        let (title, line) = prompt_words(
            &Prompt::Approve {
                amount: U256::from(1_000_000u64),
                spender: Spender::Publishing,
            },
            &names,
        );
        assert_eq!(title, "Approve 1 USDC in MetaMask");
        assert_eq!(
            line,
            "This lets the publishing contract take exactly 1 USDC of yours, and no more."
        );
        let (title, _) = prompt_words(
            &Prompt::Pay {
                fee: U256::from(1_500_000u64),
            },
            &names,
        );
        assert_eq!(title, "Pay 1.5 USDC in MetaMask");
    }
}
