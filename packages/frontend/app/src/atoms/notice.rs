//! Notice: something that changes what someone should do. Info says what
//! happens next; caution, something that may affect you; danger, something
//! that has happened and limits you, its first sentence in bold. Never a
//! coloured left border. A notice that appears after an action carries a
//! role (`status`, or `alert` for danger); a static one has none.

use ui::icons::Icon;

use super::icon::icon;
use crate::view::{El, View, el};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoticeTone {
    Info,
    Caution,
    Danger,
}

impl NoticeTone {
    fn class(self) -> &'static str {
        match self {
            NoticeTone::Info => "sh-notice--info",
            NoticeTone::Caution => "sh-notice--caution",
            NoticeTone::Danger => "sh-notice--danger",
        }
    }

    pub fn icon(self) -> Icon {
        match self {
            NoticeTone::Info => Icon::Info,
            NoticeTone::Caution => Icon::Concern,
            NoticeTone::Danger => Icon::Serious,
        }
    }
}

/// A notice with its usual icon.
pub fn notice(tone: NoticeTone, body: impl Into<View>) -> El {
    notice_with(tone, tone.icon(), body)
}

/// A notice with another icon (a clock for something that is waiting).
pub fn notice_with(tone: NoticeTone, glyph: Icon, body: impl Into<View>) -> El {
    el("div")
        .class("sh-notice")
        .class(tone.class())
        .child(icon(glyph, 20))
        .child(el("div").class("sh-notice__body").child(body.into()))
}

/// A body that leads with a sentence in bold, then the rest.
pub fn lead(first: impl Into<String>, rest: impl Into<String>) -> View {
    View::Many(vec![
        el("strong").text(first.into()).into(),
        crate::view::text(format!(" {}", rest.into())),
    ])
}
