//! `/`: the home page, every section of the design's home board in order.
//!
//! Any agent shown here (Basis Harvester, Momentum Ladder) is illustrative
//! copy about its review, baked into the page: never live data, never a
//! return.

use crate::html::{Current, Footer, Page};

pub fn page() -> Page<'static> {
    Page {
        path: "/",
        title: "Shonin · Trading agents you can check",
        description: "Reviewed code, running in an attested machine, trading with keys that can't withdraw your money. Agents trade across Hyperliquid, Solana, Base and Robinhood Chain from a single vault.",
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
