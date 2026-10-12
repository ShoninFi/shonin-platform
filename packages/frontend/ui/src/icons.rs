//! Shonin's 24px line icons, as data.
//!
//! Drawn on a 24px grid with a 1.75 stroke in viewBox units, round caps and
//! joins, no fill, `currentColor`. Each icon is a list of shapes, so the app
//! (Dominator) and the static pages (HTML text) draw the same thing from one
//! source. An icon always sits beside a word: the SVG is `aria-hidden`, and an
//! icon-only button carries its own `aria-label`.
//!
//! The fixed icons (check, concern, serious, unknown, seal, lock, pause) have
//! one meaning each and are never used for anything else.

use std::fmt::Write;

/// One SVG element of an icon: its tag and its attributes.
#[derive(Clone, Copy, Debug)]
pub struct Shape {
    pub tag: &'static str,
    pub attrs: &'static [(&'static str, &'static str)],
}

macro_rules! p {
    ($d:literal) => {
        Shape {
            tag: "path",
            attrs: &[("d", $d)],
        }
    };
}

macro_rules! circle {
    ($cx:literal, $cy:literal, $r:literal) => {
        Shape {
            tag: "circle",
            attrs: &[("cx", $cx), ("cy", $cy), ("r", $r)],
        }
    };
}

macro_rules! rect {
    ($x:literal, $y:literal, $w:literal, $h:literal, $rx:literal) => {
        Shape {
            tag: "rect",
            attrs: &[
                ("x", $x),
                ("y", $y),
                ("width", $w),
                ("height", $h),
                ("rx", $rx),
            ],
        }
    };
}

/// Every icon on the boards.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Icon {
    // Fixed meanings.
    Check,
    Concern,
    Serious,
    Unknown,
    Seal,
    Lock,
    Pause,
    // The rest of the design system's set.
    Up,
    Down,
    Clock,
    Info,
    Code,
    Exit,
    Tank,
    Bridge,
    Copy,
    External,
    Vault,
    Review,
    Machine,
    Wallet,
    // Drawn by the website design at the same grid and stroke.
    ArrowRight,
    ChevronDown,
    ChevronRight,
    Close,
    Menu,
    Search,
    Upload,
    Plus,
    Sort,
}

impl Icon {
    pub const ALL: [Icon; 30] = [
        Icon::Check,
        Icon::Concern,
        Icon::Serious,
        Icon::Unknown,
        Icon::Seal,
        Icon::Lock,
        Icon::Pause,
        Icon::Up,
        Icon::Down,
        Icon::Clock,
        Icon::Info,
        Icon::Code,
        Icon::Exit,
        Icon::Tank,
        Icon::Bridge,
        Icon::Copy,
        Icon::External,
        Icon::Vault,
        Icon::Review,
        Icon::Machine,
        Icon::Wallet,
        Icon::ArrowRight,
        Icon::ChevronDown,
        Icon::ChevronRight,
        Icon::Close,
        Icon::Menu,
        Icon::Search,
        Icon::Upload,
        Icon::Plus,
        Icon::Sort,
    ];

    /// The design system's name for it.
    pub fn name(self) -> &'static str {
        match self {
            Icon::Check => "check",
            Icon::Concern => "concern",
            Icon::Serious => "serious",
            Icon::Unknown => "unknown",
            Icon::Seal => "seal",
            Icon::Lock => "lock",
            Icon::Pause => "pause",
            Icon::Up => "up",
            Icon::Down => "down",
            Icon::Clock => "clock",
            Icon::Info => "info",
            Icon::Code => "code",
            Icon::Exit => "exit",
            Icon::Tank => "tank",
            Icon::Bridge => "bridge",
            Icon::Copy => "copy",
            Icon::External => "external",
            Icon::Vault => "vault",
            Icon::Review => "review",
            Icon::Machine => "machine",
            Icon::Wallet => "wallet",
            Icon::ArrowRight => "arrow-right",
            Icon::ChevronDown => "chevron-down",
            Icon::ChevronRight => "chevron-right",
            Icon::Close => "close",
            Icon::Menu => "menu",
            Icon::Search => "search",
            Icon::Upload => "upload",
            Icon::Plus => "plus",
            Icon::Sort => "sort",
        }
    }

    /// What it means, as the Foundations board lists it.
    pub fn meaning(self) -> &'static str {
        match self {
            Icon::Check => "Looks fine · done",
            Icon::Concern => "Some concerns",
            Icon::Serious => "Serious concerns · failed",
            Icon::Unknown => "Couldn’t tell",
            Icon::Seal => "Attested",
            Icon::Lock => "Sealed",
            Icon::Pause => "Stopped",
            Icon::Up => "Rise",
            Icon::Down => "Fall",
            Icon::Clock => "Time · waiting",
            Icon::Info => "Information",
            Icon::Code => "Source code",
            Icon::Exit => "Exit · withdraw",
            Icon::Tank => "Tank",
            Icon::Bridge => "Bridge",
            Icon::Copy => "Copy",
            Icon::External => "Another site",
            Icon::Vault => "Vault · Agents tab",
            Icon::Review => "Review",
            Icon::Machine => "Attested machine",
            Icon::Wallet => "Wallet · Portfolio tab",
            Icon::ArrowRight => "Go · next",
            Icon::ChevronDown => "Open · select",
            Icon::ChevronRight => "Drill in",
            Icon::Close => "Close · dismiss",
            Icon::Menu => "Open menu",
            Icon::Search => "Search · filter",
            Icon::Upload => "Upload · Publish tab",
            Icon::Plus => "Add",
            Icon::Sort => "Sortable column",
        }
    }

    /// Whether the icon has one meaning only and is never reused.
    pub fn is_fixed(self) -> bool {
        matches!(
            self,
            Icon::Check
                | Icon::Concern
                | Icon::Serious
                | Icon::Unknown
                | Icon::Seal
                | Icon::Lock
                | Icon::Pause
        )
    }

    /// Whether the website design added it to the design system's set.
    pub fn is_new(self) -> bool {
        matches!(
            self,
            Icon::ArrowRight
                | Icon::ChevronDown
                | Icon::ChevronRight
                | Icon::Close
                | Icon::Menu
                | Icon::Search
                | Icon::Upload
                | Icon::Plus
                | Icon::Sort
        )
    }

    /// The shapes it is drawn with.
    pub fn shapes(self) -> &'static [Shape] {
        match self {
            Icon::Check => &[p!("M5 12.5l4.5 4.5L19 7.5")],
            Icon::Concern => &[p!("M12 4.5l8.5 15h-17z"), p!("M12 10v4"), p!("M12 17h.01")],
            Icon::Serious => &[
                p!("M8.6 3.5h6.8l5.1 5.1v6.8l-5.1 5.1H8.6l-5.1-5.1V8.6z"),
                p!("M9.2 9.2l5.6 5.6M14.8 9.2l-5.6 5.6"),
            ],
            Icon::Unknown => &[
                circle!("12", "12", "8.5"),
                p!("M9.6 9.6a2.5 2.5 0 1 1 3.4 2.3c-.6.3-1 .8-1 1.5v.6"),
                p!("M12 16.8h.01"),
            ],
            Icon::Seal => &[
                rect!("3.5", "3.5", "17", "17", "4.5"),
                p!("M12 3.5v3.2M12 17.3v3.2"),
                circle!("12", "12", "3.2"),
            ],
            Icon::Lock => &[
                rect!("5", "10.5", "14", "10", "2.5"),
                p!("M8.5 10.5V8a3.5 3.5 0 0 1 7 0v2.5"),
                p!("M12 14.5v2"),
            ],
            Icon::Pause => &[circle!("12", "12", "8.5"), p!("M10 9v6M14 9v6")],
            Icon::Up => &[p!("M7 17L17 7"), p!("M9 7h8v8")],
            Icon::Down => &[p!("M7 7l10 10"), p!("M17 9v8H9")],
            Icon::Clock => &[circle!("12", "12", "8.5"), p!("M12 7.5V12l3 2")],
            Icon::Info => &[
                circle!("12", "12", "8.5"),
                p!("M12 11v5.5"),
                p!("M12 7.8h.01"),
            ],
            Icon::Code => &[
                p!("M8.5 7.5L4 12l4.5 4.5"),
                p!("M15.5 7.5L20 12l-4.5 4.5"),
                p!("M13.2 5.5l-2.4 13"),
            ],
            Icon::Exit => &[
                p!("M14 4.5h4a1.5 1.5 0 0 1 1.5 1.5v12a1.5 1.5 0 0 1-1.5 1.5h-4"),
                p!("M10 8l-4 4 4 4"),
                p!("M6 12h9"),
            ],
            Icon::Tank => &[
                rect!("5.5", "3.5", "13", "17", "3"),
                p!("M5.5 13h13"),
                p!("M9 16.5h6"),
                p!("M18.5 8H20v6"),
            ],
            Icon::Bridge => &[
                p!("M3.5 17.5h17"),
                p!("M5 17.5c1.5-5 4-7.5 7-7.5s5.5 2.5 7 7.5"),
                p!("M8.5 11.9v5.6M12 10v7.5M15.5 11.9v5.6"),
            ],
            Icon::Copy => &[
                rect!("8.5", "8.5", "11", "11", "2.5"),
                p!(
                    "M15.5 8.5V6a1.5 1.5 0 0 0-1.5-1.5H6A1.5 1.5 0 0 0 4.5 6v8A1.5 1.5 0 0 0 6 15.5h2.5"
                ),
            ],
            Icon::External => &[
                p!("M13.5 4.5h6v6"),
                p!("M19.5 4.5l-8 8"),
                p!(
                    "M17 13.5v4.5a1.5 1.5 0 0 1-1.5 1.5h-9A1.5 1.5 0 0 1 5 18V8.5A1.5 1.5 0 0 1 6.5 7H11"
                ),
            ],
            Icon::Vault => &[
                rect!("3.5", "4.5", "17", "15", "3"),
                circle!("12", "12", "3.5"),
                p!("M12 8.5V7M12 17v-1.5M8.5 12H7M17 12h-1.5"),
                p!("M6.5 19.5v1.5M17.5 19.5v1.5"),
            ],
            Icon::Review => &[
                p!("M6.5 3.5h7l4 4v4"),
                p!("M13.5 3.5v4h4"),
                p!("M6.5 3.5a1.5 1.5 0 0 0-1.5 1.5v14a1.5 1.5 0 0 0 1.5 1.5H10"),
                circle!("15.5", "15.5", "3.5"),
                p!("M18 18l2.5 2.5"),
            ],
            Icon::Machine => &[
                rect!("6.5", "6.5", "11", "11", "2"),
                rect!("9.75", "9.75", "4.5", "4.5", "1"),
                p!(
                    "M9.5 3.5v3M14.5 3.5v3M9.5 17.5v3M14.5 17.5v3M3.5 9.5h3M3.5 14.5h3M17.5 9.5h3M17.5 14.5h3"
                ),
            ],
            Icon::Wallet => &[
                p!(
                    "M18.5 7.5V6a1.5 1.5 0 0 0-1.5-1.5H6A2.5 2.5 0 0 0 3.5 7v10A2.5 2.5 0 0 0 6 19.5h12.5A1.5 1.5 0 0 0 20 18v-9a1.5 1.5 0 0 0-1.5-1.5H6a2.5 2.5 0 0 1-2.5-2.5"
                ),
                Shape {
                    tag: "circle",
                    attrs: &[
                        ("cx", "16"),
                        ("cy", "13.5"),
                        ("r", "1"),
                        ("fill", "currentColor"),
                        ("stroke", "none"),
                    ],
                },
            ],
            Icon::ArrowRight => &[p!("M5 12h14"), p!("M13 6l6 6-6 6")],
            Icon::ChevronDown => &[p!("M6 9l6 6 6-6")],
            Icon::ChevronRight => &[p!("M9 6l6 6-6 6")],
            Icon::Close => &[p!("M6 6l12 12M18 6L6 18")],
            Icon::Menu => &[p!("M4 7h16M4 12h16M4 17h16")],
            Icon::Search => &[circle!("11", "11", "6.5"), p!("M16 16l4 4")],
            Icon::Upload => &[
                p!("M12 15V4"),
                p!("M7 9l5-5 5 5"),
                p!("M4.5 15v3.5a1.5 1.5 0 0 0 1.5 1.5h12a1.5 1.5 0 0 0 1.5-1.5V15"),
            ],
            Icon::Plus => &[p!("M12 5v14M5 12h14")],
            Icon::Sort => &[p!("M8 5v14M5 16l3 3 3-3"), p!("M16 19V5M13 8l3-3 3 3")],
        }
    }

    /// The icon as SVG markup, `aria-hidden`, at `size` pixels, with extra
    /// classes on the `<svg>` (`sh-icon` is always there).
    pub fn svg(self, size: u32, class: &str) -> String {
        let mut out = format!(
            "<svg class=\"sh-icon{}{class}\" width=\"{size}\" height=\"{size}\" viewBox=\"0 0 24 24\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.75\" stroke-linecap=\"round\" stroke-linejoin=\"round\" aria-hidden=\"true\" focusable=\"false\">",
            if class.is_empty() { "" } else { " " }
        );
        for shape in self.shapes() {
            let _ = write!(out, "<{}", shape.tag);
            for (name, value) in shape.attrs {
                let _ = write!(out, " {name}=\"{value}\"");
            }
            out.push_str("/>");
        }
        out.push_str("</svg>");
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_has_a_unique_name_and_something_to_draw() {
        let mut names: Vec<&str> = Icon::ALL.iter().map(|icon| icon.name()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), Icon::ALL.len());
        for icon in Icon::ALL {
            assert!(!icon.shapes().is_empty(), "{}", icon.name());
            assert!(!icon.meaning().is_empty());
        }
    }

    #[test]
    fn the_set_is_the_design_system_s_twenty_one_plus_the_nine_new_ones() {
        assert_eq!(Icon::ALL.iter().filter(|icon| icon.is_new()).count(), 9);
        assert_eq!(Icon::ALL.iter().filter(|icon| !icon.is_new()).count(), 21);
        assert_eq!(Icon::ALL.iter().filter(|icon| icon.is_fixed()).count(), 7);
    }

    #[test]
    fn an_icon_is_hidden_from_screen_readers_and_inherits_its_colour() {
        let svg = Icon::Check.svg(20, "x");
        assert!(svg.starts_with("<svg class=\"sh-icon x\" width=\"20\" height=\"20\""));
        assert!(svg.contains("aria-hidden=\"true\""));
        assert!(svg.contains("stroke=\"currentColor\""));
        assert!(svg.contains("<path d=\"M5 12.5l4.5 4.5L19 7.5\"/>"));
        assert!(svg.ends_with("</svg>"));
    }
}
