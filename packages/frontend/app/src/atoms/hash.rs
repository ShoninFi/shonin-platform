//! Hash: a hash or an address someone may want to copy and compare. Shown as
//! `0x` and the first 4 … last 4 in mono; the full value is in the title and
//! read out in full; the copy button (24px, with a 44px hit area) puts the
//! full value on the clipboard, turns into a check for 1.5 s and says
//! "Copied" politely.

use futures_signals::signal::Mutable;
use ui::icons::Icon;

use super::icon::icon;
use crate::format::short_hash;
use crate::view::{El, el};

/// A hash chip. `label` is the small word before it ("build"); `what` names
/// it for the copy button ("transaction hash").
pub fn hash(full: &str, label: Option<&'static str>, what: &'static str) -> El {
    let copied = Mutable::new(false);
    let value = full.to_string();
    let chip = el("span").class("sh-hash").attr("title", full.to_string());
    let chip = match label {
        Some(label) => chip.child(el("span").class("sh-hash__label").text(label)),
        None => chip,
    };
    chip.child(
        el("span")
            .class("sh-hash__value")
            .child(
                el("span")
                    .attr("aria-hidden", "true")
                    .text(short_hash(full)),
            )
            .child(el("span").class("sh-vh").text(full.to_string())),
    )
    .child(
        el("button")
            .attr("type", "button")
            .class("sh-hash__copy")
            .attr("aria-label", format!("Copy {what} {full}"))
            .attr_from("data-copied", &copied, |copied| copied.then(String::new))
            .child_from(&copied, |copied| {
                Some(icon(if *copied { Icon::Check } else { Icon::Copy }, 14).into())
            })
            .hook({
                let copied = copied.clone();
                move |dom| {
                    dom.event(move |_: dominator::events::Click| {
                        crate::web::copy(&value);
                        copied.set(true);
                        let copied = copied.clone();
                        gloo_timers::callback::Timeout::new(1_500, move || copied.set(false))
                            .forget();
                    })
                }
            }),
    )
    .child_from(&copied, |copied| {
        copied.then(|| {
            el("span")
                .class("sh-tip")
                .attr("aria-hidden", "true")
                .text("Copied")
                .into()
        })
    })
    .child(
        el("span")
            .class("sh-vh")
            .attr("role", "status")
            .text_from(&copied, |copied| {
                if *copied {
                    "Copied".into()
                } else {
                    String::new()
                }
            }),
    )
}

/// A hash shown whole-in-short, with no copy button (where a copy button
/// would be noise, as in a dense list).
pub fn hash_plain(full: &str) -> El {
    el("span")
        .class("sh-hash")
        .class("sh-hash--plain")
        .attr("title", full.to_string())
        .child(
            el("span")
                .class("sh-hash__value")
                .child(
                    el("span")
                        .attr("aria-hidden", "true")
                        .text(short_hash(full)),
                )
                .child(el("span").class("sh-vh").text(full.to_string())),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::View;

    #[test]
    fn a_hash_shows_short_but_reads_and_copies_in_full() {
        let full = "0x4e1d30877432d1026706d7e805da846a32c3bb81e3c29b62179273c8eb5ba90b";
        let view: View = hash(full, Some("build"), "build hash").into();
        let html = view.html();
        assert!(html.contains(&format!("title=\"{full}\"")));
        assert!(html.contains("0x4e1d\u{2026}a90b"));
        assert!(html.contains(&format!("<span class=\"sh-vh\">{full}</span>")));
        assert!(html.contains(&format!("aria-label=\"Copy build hash {full}\"")));
        assert!(html.contains("role=\"status\""));
    }
}
