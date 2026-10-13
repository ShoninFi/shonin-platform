//! The retire dialog (Retire-Desktop): what the week's wind-down means, in a
//! table; the agent's name typed to confirm, with "Retire agent" disabled
//! until it matches; "Keep it running" focused first, and Esc keeps it
//! running. When the tank covers less than the wind-down, the caution
//! variant says the agent will stop early.

use std::rc::Rc;

use futures_signals::signal::{Mutable, SignalExt};

use super::Read;
use crate::atoms::dialog::{self, Dialog};
use crate::atoms::notice::{NoticeTone, notice};
use crate::money::format as money;
use crate::pages::publish::parts::{self, Names};
use crate::tx::author::Step;
use crate::tx::author::form;
use crate::tx::author::run::{Action, Running};
use crate::view::{View, el, text};

/// Whole days in a span of seconds, rounded down.
pub fn whole_days(seconds: u64) -> u64 {
    seconds / 86_400
}

/// The sentence about the tank and the wind-down, and whether it's the
/// caution variant (the tank covers fewer days than the wind-down).
pub(crate) fn coverage(read: &Read) -> (String, bool) {
    let week = whole_days(read.at.wind_down).max(1);
    let decimals = read.network.asset_decimals;
    match read.daily {
        Some(daily) if !daily.is_zero() => {
            let days = form::runway_days(read.at.tank.balance, daily).unwrap_or_default();
            let short = form::covers_less_than(read.at.tank.balance, daily, week);
            (
                format!(
                    "Your tank covers at least {days} days at the allowances' upper bound of {} a day. The wind-down needs {week}.",
                    money::dollars_exact(daily, decimals)
                ),
                short,
            )
        }
        _ => (format!("The wind-down needs {week} days."), false),
    }
}

pub(crate) fn open(read: &Rc<Read>, slot: &Mutable<Option<Running>>, names: &Rc<Names>) {
    let name = read.title.clone();
    let typed = Mutable::new(String::new());
    let (sentence, short) = coverage(read);
    let week = whole_days(read.at.wind_down).max(1);
    let table = el("table")
        .class("sh-table sh-retire__table")
        .child(el("caption").class("sh-vh").text("What happens when you retire"))
        .child(
            el("thead").child(
                el("tr")
                    .child(el("th").attr("scope", "col").text("When"))
                    .child(el("th").attr("scope", "col").text("Deposits"))
                    .child(el("th").attr("scope", "col").text("Your agent"))
                    .child(el("th").attr("scope", "col").text("Withdrawals")),
            ),
        )
        .child(
            el("tbody")
                .child(row(&["The moment you retire", "Stop at once", "Keeps running", "Keep working"]))
                .child(row(&[
                    &format!("The next {week} days"),
                    "Stay closed",
                    "Keeps running, as long as the tank lasts",
                    "Keep working",
                ]))
                .child(row(&[
                    &format!("After {week} days"),
                    "Closed for good",
                    "Stopped for good. Positions are closed and the money comes home; the costs come from the money itself.",
                    "Keep working. The vault stays on-chain forever, holding cash.",
                ])),
        );
    let early = "If the tank can't cover the week, the agent stops early and its positions sit untouched until the week is up.";
    let tank_line: View = if short {
        notice(
            NoticeTone::Caution,
            View::Many(vec![
                el("strong").text(sentence).into(),
                text(format!(" {early}")),
            ]),
        )
        .into()
    } else {
        View::Many(vec![
            el("p").text(sentence).into(),
            el("p").class("sh-muted").text(early).into(),
        ])
    };
    let match_name = name.clone();
    let hint_name = name.clone();
    let body = el("div")
        .class("sh-retire")
        .child(el("p").text(format!(
            "Retiring takes {week} days, on purpose, so Depositors get a week's warning. You can't undo it."
        )))
        .child(table)
        .child(tank_line)
        .child(
            el("div")
                .class("sh-field")
                .child(
                    el("label")
                        .class("sh-field__label")
                        .attr("for", "retire-name")
                        .text("Type the agent's name to confirm"),
                )
                .child(
                    el("div").class("sh-input").child(
                        el("input")
                            .attr("id", "retire-name")
                            .attr("type", "text")
                            .attr("autocomplete", "off")
                            .attr("spellcheck", "false")
                            .attr("aria-describedby", "retire-name-hint")
                            .hook({
                                let typed = typed.clone();
                                move |dom| {
                                    dominator::with_node!(dom, input => {
                                        .event(move |_: dominator::events::Input| {
                                            if let Some(input) = wasm_bindgen::JsCast::dyn_ref::<web_sys::HtmlInputElement>(&input) {
                                                typed.set_neq(input.value());
                                            }
                                        })
                                    })
                                }
                            }),
                    ),
                )
                .child(
                    el("p")
                        .class("sh-field__hint")
                        .attr("id", "retire-name-hint")
                        .attr("aria-live", "polite")
                        .text_signal(
                            format!("Type “{hint_name}”."),
                            typed.signal_cloned().map(move |typed| {
                                if form::name_matches(&typed, &hint_name) {
                                    "The name matches.".to_string()
                                } else {
                                    format!("Type “{hint_name}”.")
                                }
                            }),
                        ),
                ),
        )
        .child(el("div").child_signal(
            None,
            slot.signal_cloned().map({
                let names = names.clone();
                move |running| running.map(|running| parts::status(&running, names.clone()).into())
            }),
        ));

    let keep = el("button")
        .attr("type", "button")
        .class("sh-btn sh-btn-secondary")
        .text("Keep it running")
        .hook(|dom| {
            dom.event(|_: dominator::events::Click| dialog::close())
                .after_inserted(|element| {
                    // After the dialog has placed focus on its first field
                    // (which it does a tick after it is inserted).
                    wasm_bindgen_futures::spawn_local(async move {
                        gloo_timers::future::TimeoutFuture::new(0).await;
                        gloo_timers::future::TimeoutFuture::new(0).await;
                        if let Some(element) =
                            wasm_bindgen::JsCast::dyn_ref::<web_sys::HtmlElement>(&element)
                        {
                            crate::web::focus(element);
                        }
                    });
                })
        });
    let retire_read = read.clone();
    let retire_slot = slot.clone();
    let retire_typed = typed.clone();
    let enabled = futures_signals::map_ref! {
        let typed = typed.signal_cloned(),
        let running = slot.signal_cloned() => {
            let live = running.as_ref().is_some_and(Running::is_live);
            form::name_matches(typed, &match_name) && !live
        }
    };
    let retire = el("button")
        .attr("type", "button")
        .class("sh-btn sh-btn-danger")
        .attr("disabled", "")
        .attr_signal(
            "disabled",
            Some(String::new()),
            enabled.map(|enabled| (!enabled).then(String::new)),
        )
        .text("Retire agent")
        .hook(move |dom| {
            dom.event(move |_: dominator::events::Click| {
                if !form::name_matches(&retire_typed.get_cloned(), &retire_read.title) {
                    return;
                }
                if retire_slot
                    .lock_ref()
                    .as_ref()
                    .is_some_and(Running::is_live)
                {
                    return;
                }
                let running = Running::new();
                retire_slot.set(Some(running.clone()));
                // The dialog stays while the wallet is asked.
                dialog::lock(true);
                let unlock = running.step.signal_cloned().for_each(|step| {
                    if matches!(step, Some(Step::Done(_) | Step::Failed(_))) {
                        dialog::lock(false);
                    }
                    async {}
                });
                wasm_bindgen_futures::spawn_local(unlock);
                crate::tx::author::run::start(
                    retire_read.chain,
                    Action::Retire {
                        vault: retire_read.vault,
                    },
                    retire_read.network.config,
                    &running,
                    |_| {},
                );
            })
        });
    dialog::open(Dialog {
        title: format!("Retire {name}?"),
        body: body.into(),
        foot: Some(View::Many(vec![keep.into(), retire.into()])),
        wide: true,
    });
}

fn row(cells: &[&str]) -> crate::view::El {
    let mut row = el("tr");
    for (at, cell) in cells.iter().enumerate() {
        row = row.child(if at == 0 {
            el("th").attr("scope", "row").text(cell.to_string())
        } else {
            el("td").text(cell.to_string())
        });
    }
    row
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wind_down_is_counted_in_whole_days() {
        assert_eq!(whole_days(7 * 86_400), 7);
        assert_eq!(whole_days(7 * 86_400 - 1), 6);
    }
}
