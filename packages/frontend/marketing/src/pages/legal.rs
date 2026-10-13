//! `/terms` and `/privacy`, in the docs' plain voice.
//!
//! The terms carry, word for word, the statement a depositor's wallet signs
//! before a first deposit (the vault contract's own text), and the restricted
//! codes the contracts carry today. Both pages are drafts for the owner to
//! review; neither claims to be advice of any kind.

use crate::html::{Current, Footer, Page};

/// The statement every depositor signs, exactly as the vault contracts hold
/// it. A test checks it against the EVM vault's source.
pub const STATEMENT: &str =
    "I am not a restricted person for this vault, and I am not acting for one.";

pub fn terms() -> Page<'static> {
    Page {
        path: "/terms",
        title: "Terms of Use · Shonin",
        description: "Who Shonin isn't offered to, what it is and isn't, and what you take on when you use it.",
        current: Current::Other,
        class: "page-legal",
        main: TERMS,
        footer: Footer::Full,
        indexed: true,
    }
}

pub fn privacy() -> Page<'static> {
    Page {
        path: "/privacy",
        title: "Privacy Policy · Shonin",
        description: "No accounts and no cookies. Cookieless, aggregate analytics from Cloudflare. Your wallet address and your transactions are public on-chain.",
        current: Current::Other,
        class: "page-legal",
        main: PRIVACY,
        footer: Footer::Full,
        indexed: true,
    }
}

const TERMS: &str = include_str!("legal/terms.html");
const PRIVACY: &str = include_str!("legal/privacy.html");
