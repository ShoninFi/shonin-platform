//! `/`: the home page, every section of the design's home board in order.
//!
//! Any agent shown here (Basis Harvester, Momentum Ladder) is illustrative
//! copy about its review, baked into the page: never live data, never a
//! return.

use crate::html::{Current, Footer, Page};

pub fn page() -> Page<'static> {
    Page {
        path: "/",
        title: "Shonin · Trading agents. Verifiable by design",
        description: "Attested execution. AI reviews, even for sealed code. Powerful off-chain freedom. Enforced safeguards for your capital. Agents trade across protocols on Hyperliquid, Solana, Base and Robinhood Chain, with more to come.",
        current: Current::Home,
        class: "page-home",
        main: MAIN,
        footer: Footer::Full,
    }
}

const MAIN: &str = concat!(
    include_str!("index/hero.html"),
    include_str!("index/how.html"),
    include_str!("index/traders.html"),
    include_str!("index/authors.html"),
    include_str!("index/publishing.html"),
    include_str!("index/fees.html"),
    include_str!("index/questions.html"),
    include_str!("index/risks.html"),
    include_str!("index/cta.html"),
);
