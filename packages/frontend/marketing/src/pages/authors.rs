//! `/authors`: For Authors. Publish an agent for 1 USDC, earn 10% of new profit.
//!
//! The Rust in the hero uses the strategy SDK's own names; the agent, its
//! allowances and its earnings are illustrations, labelled as such.

use crate::html::{Current, Footer, Page};

pub fn page() -> Page<'static> {
    Page {
        path: "/authors",
        title: "For Authors · Shonin",
        description: "Write a trading agent, publish it for a flat 1 USDC, and earn 10% of the new profit it makes on Depositors' money. You don't need anyone's permission.",
        current: Current::Authors,
        class: "page-authors",
        main: MAIN,
        footer: Footer::Full,
        indexed: true,
    }
}

const MAIN: &str = concat!(
    include_str!("authors/hero.html"),
    include_str!("authors/write.html"),
    include_str!("authors/cost.html"),
    include_str!("authors/earn.html"),
    include_str!("authors/steps.html"),
    include_str!("authors/review.html"),
    include_str!("authors/start.html"),
    include_str!("authors/private.html"),
    include_str!("authors/done.html"),
    include_str!("authors/cta.html"),
);
