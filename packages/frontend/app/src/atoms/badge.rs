//! Badge: a chip for a short status or fact beside what it describes. Never a
//! button. Status colour always comes with a word or an icon.

use ui::icons::Icon;

use super::icon::icon;
use crate::view::{El, el};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Gain,
    Caution,
    Loss,
    Neutral,
    Brand,
}

impl Tone {
    pub fn class(self) -> &'static str {
        match self {
            Tone::Gain => "sh-chip-gain",
            Tone::Caution => "sh-chip-caution",
            Tone::Loss => "sh-chip-loss",
            Tone::Neutral => "sh-chip-neutral",
            Tone::Brand => "sh-chip-brand",
        }
    }
}

/// A chip: tone, an optional 14px icon, and its words.
pub fn badge(tone: Tone, glyph: Option<Icon>, words: impl Into<String>) -> El {
    let chip = el("span").class("sh-chip").class(tone.class());
    let chip = match glyph {
        Some(glyph) => chip.child(icon(glyph, 14)),
        None => chip,
    };
    chip.text(words.into())
}
