//! The shutdown notice: what app.shonin.fi shows, at every address, now
//! that the app is shut down. Not one of the marketing site's pages
//! ([`super::all`]): it is built on its own, into the app's dist, by
//! [`crate::shutdown`].

use crate::html::{Current, Footer, Page};

pub fn page() -> Page<'static> {
    Page {
        path: "/",
        title: "Shonin is shut down",
        description: "As of 8 November 2026 the Shonin app is shut down, and its services and test networks are retired. It never launched on mainnet, so no deposits are held and there is nothing to withdraw. The docs and the code stay up.",
        // The notice draws its own header and footer (html::notice).
        current: Current::Other,
        class: "page-shutdown",
        main: MAIN,
        footer: Footer::Full,
        // Served at every address, so it names none and asks not to be listed.
        indexed: false,
    }
}

const MAIN: &str = include_str!("shutdown/notice.html");
