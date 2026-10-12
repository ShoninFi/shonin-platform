//! `/portfolio` and `/publish`: the routes exist, with their place in the
//! navigation and the tab bar, and say plainly that what they hold arrives
//! with the wallet in the next update. Neither is indexed.

use ui::icons::Icon;

use crate::atoms::icon;
use crate::atoms::notice::{NoticeTone, notice};
use crate::atoms::skeleton::skeleton_block;
use crate::route::DOCS;
use crate::view::{View, el, text};

use super::Page;

fn page(title: &'static str, lede: &'static str, body: View) -> Page {
    Page {
        top: None,
        body: el("div")
            .class("soon")
            .child(
                el("div")
                    .class("soon-head")
                    .child(el("h1").class("soon-title").text(title))
                    .child(el("p").class("soon-lede").text(lede)),
            )
            .child(notice(NoticeTone::Info, body))
            .child(skeleton_block("100%", 160).class("soon-skel"))
            .into(),
    }
}

pub fn portfolio() -> Page {
    page(
        "Portfolio",
        "Your shares, what they’re worth and any booked withdrawals.",
        text(
            "Connect a wallet to see your positions here. Wallet connections arrive in the next update of this app; every other page reads without one.",
        ),
    )
}

pub fn publish() -> Page {
    page(
        "Publish an agent",
        "Write an agent in Rust and publish it for a flat 1 USDC: reviewed, built in an attested machine, then yours to activate.",
        View::Many(vec![
            text(
                "Publishing from this app arrives in its next update. Until then, the docs explain how an agent is packaged and published. ",
            ),
            el("a")
                .attr("href", format!("{DOCS}writing/publishing/"))
                .text("Read the guide")
                .child(icon(Icon::External, 14))
                .into(),
        ]),
    )
}
