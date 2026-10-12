//! Logo: the files exactly as drawn, always an image with alt "Shonin",
//! always in a link to shonin.fi labelled "Shonin home".

use crate::route::SITE;
use crate::view::{El, el};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Logo {
    /// On paper: the headers. 28px tall (24 on phone).
    Lockup,
    /// The mark alone: the phone header beside a page title.
    Mark,
    /// On navy.
    LockupReversed,
    /// With the tagline, on navy: the footer band.
    TaglineReversed,
}

impl Logo {
    pub fn file(self) -> &'static str {
        match self {
            Logo::Lockup => "/logos/shonin-lockup.svg",
            Logo::Mark => "/logos/shonin-mark.svg",
            Logo::LockupReversed => "/logos/shonin-lockup-reversed.svg",
            Logo::TaglineReversed => "/logos/shonin-lockup-tagline-reversed.svg",
        }
    }

    fn class(self) -> &'static str {
        match self {
            Logo::Lockup | Logo::LockupReversed => "sh-logo--lockup",
            Logo::Mark => "sh-logo--mark",
            Logo::TaglineReversed => "sh-logo--tagline",
        }
    }

    /// Intrinsic size, so the page doesn't move when the file arrives.
    fn size(self) -> (&'static str, &'static str) {
        match self {
            Logo::Lockup | Logo::LockupReversed => ("87", "28"),
            Logo::Mark => ("26", "28"),
            Logo::TaglineReversed => ("124", "40"),
        }
    }
}

/// The logo file as an image.
pub fn image(logo: Logo) -> El {
    let (width, height) = logo.size();
    el("img")
        .attr("src", logo.file())
        .attr("alt", "Shonin")
        .attr("width", width)
        .attr("height", height)
}

/// The logo in its link home, to shonin.fi.
pub fn home(logo: Logo) -> El {
    el("a")
        .class("sh-logo")
        .class(logo.class())
        .attr("href", SITE)
        .attr("aria-label", "Shonin home")
        .child(image(logo))
}
