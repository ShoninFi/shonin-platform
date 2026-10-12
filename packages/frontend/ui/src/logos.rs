//! The logo files, exactly as the design system draws them. Never recoloured,
//! flattened, rotated, stretched or retyped: always the file.

/// The lockup on paper (headers). 3.09 : 1.
pub const LOCKUP: &str = include_str!("../logos/shonin-lockup.svg");
/// The lockup on navy (the footer band, the hero). 3.09 : 1.
pub const LOCKUP_REVERSED: &str = include_str!("../logos/shonin-lockup-reversed.svg");
/// The lockup with the tagline, on navy: the footer band's large moment.
pub const LOCKUP_TAGLINE_REVERSED: &str =
    include_str!("../logos/shonin-lockup-tagline-reversed.svg");
/// The mark alone: the phone header, the favicon. 0.92 : 1.
pub const MARK: &str = include_str!("../logos/shonin-mark.svg");

/// Every logo file by the name a build serves it under, in `/logos/`.
pub const FILES: &[(&str, &str)] = &[
    ("shonin-lockup.svg", LOCKUP),
    ("shonin-lockup-reversed.svg", LOCKUP_REVERSED),
    (
        "shonin-lockup-tagline-reversed.svg",
        LOCKUP_TAGLINE_REVERSED,
    ),
    ("shonin-mark.svg", MARK),
];

/// Width over height of the lockups.
pub const LOCKUP_ASPECT: f64 = 1305.0 / 422.0;
/// Width over height of the mark.
pub const MARK_ASPECT: f64 = 388.5 / 422.0;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_logo_is_a_plain_svg_with_no_script_or_outside_reference() {
        for (name, svg) in FILES {
            assert!(svg.trim_start().starts_with("<svg"), "{name}");
            let lower = svg.to_ascii_lowercase();
            for banned in ["<script", "onload", "href=\"http", "foreignobject"] {
                assert!(!lower.contains(banned), "{name} holds {banned}");
            }
        }
    }
}
