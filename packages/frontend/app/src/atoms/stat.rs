//! Stat: one headline number over its period, in a row of three to five.
//! The period is never optional; colour goes on the figure only, with its
//! sign. Live updates replace the figure in place: no count-up, no flash.

use crate::view::{El, View, el};

/// A stat: its label, the figure, and the period the figure covers.
pub fn stat(label: &'static str, figure: impl Into<View>, period: impl Into<View>) -> El {
    el("div")
        .class("sh-stat")
        .child(el("p").class("sh-label").text(label))
        .child(el("p").class("sh-stat__value").child(figure.into()))
        .child(el("p").class("sh-stat__period").child(period.into()))
}

/// The same, as a tile with its own hairline card.
pub fn stat_tile(label: &'static str, figure: impl Into<View>, period: impl Into<View>) -> El {
    stat(label, figure, period).class("sh-stat--tile")
}
