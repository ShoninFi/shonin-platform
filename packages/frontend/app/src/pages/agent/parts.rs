//! Small pieces the agent page's tabs share: a card with its heading, a
//! link out to a chain's explorer, a hash with its explorer link, the "can
//! lose money" line.

use shared::Chain;
use ui::icons::Icon;

use crate::atoms::hash::hash;
use crate::atoms::icon;
use crate::store;
use crate::view::{El, View, el};

/// A card for one topic: a heading (with an optional icon), then its body.
pub fn card(id: &'static str, title: &'static str, glyph: Option<Icon>) -> El {
    let heading = el("h2").class("ag-card__title").attr("id", id);
    let heading = match glyph {
        Some(glyph) => heading.child(icon(glyph, 22)),
        None => heading,
    };
    el("section")
        .class("sh-card")
        .class("ag-card")
        .attr("aria-labelledby", id)
        .child(heading.text(title))
}

/// A card's heading row: the heading on the left, something on the right.
pub fn card_with_aside(
    id: &'static str,
    title: &'static str,
    glyph: Option<Icon>,
    aside: impl Into<View>,
) -> El {
    let heading = el("h2").class("ag-card__title").attr("id", id);
    let heading = match glyph {
        Some(glyph) => heading.child(icon(glyph, 22)),
        None => heading,
    };
    el("section")
        .class("sh-card")
        .class("ag-card")
        .attr("aria-labelledby", id)
        .child(
            el("div")
                .class("ag-card__head")
                .child(heading.text(title))
                .child(aside.into()),
        )
}

/// A standalone link with its arrow.
pub fn more(href: impl Into<String>, words: &'static str) -> El {
    el("a")
        .class("sh-link")
        .attr("href", href.into())
        .text(words)
        .child(icon(Icon::ArrowRight, 18))
}

/// A standalone link inside the app.
pub fn more_in_app(href: impl Into<String>, words: &'static str) -> El {
    crate::web::app_link(href)
        .class("sh-link")
        .text(words)
        .child(icon(Icon::ArrowRight, 18))
}

/// Which network a vault chain is on in this snapshot (`evm:84532`).
pub fn network(chain: Chain) -> Option<String> {
    store::get()
        .chains
        .lock_ref()
        .iter()
        .find(|read| read.chain == chain)
        .map(|read| read.key.clone())
}

/// A transaction on its chain's explorer, opening in a new tab, or the hash
/// alone where the chain has no explorer (a local chain).
pub fn tx_link(chain: Chain, reference: &str, label: &'static str) -> El {
    match network(chain).and_then(|key| super::facts::tx_url(&key, reference)) {
        Some(url) => el("a")
            .class("ag-tx")
            .attr("href", url)
            .attr("target", "_blank")
            .attr("rel", "noopener noreferrer")
            .attr(
                "aria-label",
                format!("{label} {reference} (opens the block explorer)"),
            )
            .child(
                el("span")
                    .class("sh-mono-s")
                    .attr("aria-hidden", "true")
                    .text(crate::format::short_hash(reference)),
            )
            .child(icon(Icon::External, 14)),
        None => el("span")
            .class("ag-tx")
            .class("sh-mono-s")
            .attr("title", reference.to_string())
            .text(crate::format::short_hash(reference)),
    }
}

/// An address on its chain's explorer, or nothing where there is none.
pub fn address_url(chain: Chain, address: &str) -> Option<String> {
    network(chain).and_then(|key| super::facts::address_url(&key, address))
}

/// A hash chip with a copy button.
pub fn chip(full: &str, what: &'static str) -> El {
    hash(full, None, what)
}

/// The line that always sits by a money action.
pub fn risk_line() -> El {
    el("p")
        .class("ag-risk")
        .child(icon(Icon::Concern, 16))
        .text("The agent can lose money. Nothing here stops that.")
}
