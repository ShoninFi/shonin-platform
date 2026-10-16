//! The test-network line: one quiet sentence above the header on every page
//! of a build that names a test network, saying that nothing here is real
//! money and no result here is a track record. Not dismissible, and never on
//! a build for a real network: the build decides, not the data.

use ui::icons::Icon;

use crate::atoms::icon;
use crate::view::{View, el};

/// What the line says.
pub const TEXT: &str =
    "Test networks: nothing here is real money, and no result here is a track record.";

/// Whether this build shows the line: one that names a test network
/// (`SHONIN_APP_NETWORK=testnet` or `local` at build time), a development
/// build, and the crate's own tests.
pub const SHOWN: bool =
    cfg!(any(test, feature = "dev")) || shown_for(option_env!("SHONIN_APP_NETWORK"));

/// Whether a build naming `network` shows the line: the same test networks
/// that offer the recovery-phrase wallet.
pub const fn shown_for(network: Option<&str>) -> bool {
    crate::wallet::dev::phrase_wallet_for(network)
}

/// The line, on a build that shows it.
pub fn line() -> View {
    if !SHOWN {
        return View::Empty;
    }
    el("aside")
        .class("sh-testnet")
        .attr("aria-label", "Test networks")
        .child(
            el("div")
                .class("sh-container")
                .class("sh-testnet__inner")
                .child(icon(Icon::Info, 16))
                .child(el("p").text(TEXT)),
        )
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_line_is_shown_only_by_a_build_that_names_a_test_network() {
        assert!(shown_for(Some("testnet")));
        assert!(shown_for(Some("local")));
        assert!(!shown_for(Some("mainnet")));
        assert!(!shown_for(Some("")));
        assert!(!shown_for(None));
    }
}
