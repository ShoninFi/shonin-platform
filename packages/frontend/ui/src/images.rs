//! The PNG images both sites serve: the share images link previews show
//! (Open Graph and X cards), and the icons drawn from the mark for the
//! places an SVG favicon can't go (home screens, older browsers, search
//! results).
//!
//! Each is drawn from an SVG source in `images/` on the tokens' colours and
//! the self-hosted fonts, rasterised by `images/render.sh`, and committed.
//! Change the source and rerun the script; never edit a PNG by hand.

/// One PNG, with the size it is declared at (a test reads the file's own
/// header and checks it).
pub struct Png {
    /// The file name it is served under.
    pub name: &'static str,
    pub bytes: &'static [u8],
    pub width: u32,
    pub height: u32,
}

/// A share image, and what it shows in words, for `og:image:alt`.
pub struct Share {
    pub png: Png,
    pub alt: &'static str,
}

/// shonin.fi's share image: the home hero's eyebrow, headline and the mark.
pub const SHARE_SITE: Share = Share {
    png: Png {
        name: "shonin.png",
        bytes: include_bytes!("../images/share/shonin.png"),
        width: 1200,
        height: 630,
    },
    alt: "Shonin. The open platform for autonomous finance: trading agents, verifiable by design.",
};

/// app.shonin.fi's share image: the app's headline beside a sketch of the
/// agents list, with no names and no figures.
pub const SHARE_APP: Share = Share {
    png: Png {
        name: "shonin-app.png",
        bytes: include_bytes!("../images/share/shonin-app.png"),
        width: 1200,
        height: 630,
    },
    alt: "Shonin. Every agent's review, record and exit, read from its own contracts.",
};

/// The favicon for browsers that don't take the SVG one: the mark alone.
pub const ICON_32: Png = Png {
    name: "shonin-32.png",
    bytes: include_bytes!("../images/icons/shonin-32.png"),
    width: 32,
    height: 32,
};

/// The home-screen icon (`apple-touch-icon`): the mark on paper.
pub const ICON_180: Png = Png {
    name: "shonin-180.png",
    bytes: include_bytes!("../images/icons/shonin-180.png"),
    width: 180,
    height: 180,
};

/// The large icon (search results, app launchers): the mark on paper.
pub const ICON_512: Png = Png {
    name: "shonin-512.png",
    bytes: include_bytes!("../images/icons/shonin-512.png"),
    width: 512,
    height: 512,
};

/// Every icon, served together under `/icons/` (the app) or fingerprinted
/// under `/assets/icons/` (the marketing site).
pub const ICONS: [&Png; 3] = [&ICON_32, &ICON_180, &ICON_512];

/// The width and height a PNG's header declares, if it is a PNG.
pub fn dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    const SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";
    if bytes.len() < 24 || !bytes.starts_with(SIGNATURE) || &bytes[12..16] != b"IHDR" {
        return None;
    }
    let read =
        |at: usize| u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
    Some((read(16), read(20)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_image_is_the_size_it_says_and_small_enough_to_fetch() {
        for png in [&SHARE_SITE.png, &SHARE_APP.png].into_iter().chain(ICONS) {
            assert_eq!(
                dimensions(png.bytes),
                Some((png.width, png.height)),
                "{}",
                png.name
            );
            assert!(png.bytes.len() < 300 * 1024, "{} is too heavy", png.name);
        }
    }

    #[test]
    fn share_images_are_the_size_link_previews_want() {
        for share in [&SHARE_SITE, &SHARE_APP] {
            assert_eq!((share.png.width, share.png.height), (1200, 630));
            assert!(share.alt.starts_with("Shonin. "));
        }
        assert_eq!(
            ICONS.map(|icon| (icon.width, icon.height)),
            [(32, 32), (180, 180), (512, 512)]
        );
    }

    #[test]
    fn dimensions_reads_only_a_png_header() {
        assert_eq!(dimensions(b"GIF89a"), None);
        assert_eq!(dimensions(&[]), None);
    }
}
