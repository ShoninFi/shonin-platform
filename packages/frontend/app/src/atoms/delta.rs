//! Delta: a change in a figure, always signed with + or a true minus (or
//! with an arrow), in one decimal, with its colour on the figure only. Zero
//! is 0.0% in ink. A missing figure is an em dash with its reason for screen
//! readers.

use futures_signals::signal::Mutable;
use ui::icons::Icon;

use super::icon::icon;
use crate::format::{self, Direction};
use crate::view::{El, el};

fn modifier(direction: Direction) -> &'static str {
    match direction {
        Direction::Up => "sh-delta--up",
        Direction::Down => "sh-delta--down",
        Direction::Flat => "sh-delta--flat",
    }
}

/// The colour class for a figure that is a return: gain, loss or none.
pub fn tone_class(fraction: f64) -> &'static str {
    match format::direction(fraction) {
        Direction::Up => "sh-gain",
        Direction::Down => "sh-loss",
        Direction::Flat => "",
    }
}

/// A signed return: +3.2%, −6.9%, 0.0%.
pub fn delta(fraction: f64) -> El {
    el("span")
        .class("sh-delta")
        .class(modifier(format::direction(fraction)))
        .text(format::ret(fraction))
}

/// A return with an arrow beside its sign, for where a glance matters.
pub fn delta_arrow(fraction: f64) -> El {
    let direction = format::direction(fraction);
    let span = el("span").class("sh-delta").class(modifier(direction));
    let span = match direction {
        Direction::Up => span.child(icon(Icon::Up, 16)),
        Direction::Down => span.child(icon(Icon::Down, 16)),
        Direction::Flat => span,
    };
    span.text(format::ret(fraction))
}

/// A return that updates in place, or an em dash with `missing` read out
/// when there's none yet ("Needs 30 days").
pub fn delta_from(value: &Mutable<Option<f64>>, missing: &'static str) -> El {
    el("span")
        .class("sh-delta")
        .class_from("sh-delta--up", value, |v| {
            v.is_some_and(|v| format::direction(v) == Direction::Up)
        })
        .class_from("sh-delta--down", value, |v| {
            v.is_some_and(|v| format::direction(v) == Direction::Down)
        })
        .class_from("sh-delta--flat", value, |v| {
            v.is_none_or(|v| format::direction(v) == Direction::Flat)
        })
        .child_from(value, move |v| {
            Some(match v {
                Some(v) => crate::view::text(format::ret(*v)),
                None => missing_figure(missing).into(),
            })
        })
}

/// An em dash, with the reason beside it for screen readers.
pub fn missing_figure(reason: &'static str) -> El {
    el("span")
        .child(el("span").attr("aria-hidden", "true").text(format::MISSING))
        .child(el("span").class("sh-vh").text(reason))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::View;

    #[test]
    fn a_loss_reads_with_a_true_minus_and_the_loss_colour() {
        let view: View = delta(-0.062).into();
        assert_eq!(
            view.html(),
            "<span class=\"sh-delta sh-delta--down\">\u{2212}6.2%</span>"
        );
    }

    #[test]
    fn zero_is_ink_with_no_sign() {
        let view: View = delta(0.0).into();
        assert_eq!(
            view.html(),
            "<span class=\"sh-delta sh-delta--flat\">0.0%</span>"
        );
    }

    #[test]
    fn a_missing_return_says_why_to_a_screen_reader() {
        let view: View = delta_from(&Mutable::new(None), "Needs 30 days").into();
        let html = view.html();
        assert!(html.contains("<span aria-hidden=\"true\">—</span>"));
        assert!(html.contains("<span class=\"sh-vh\">Needs 30 days</span>"));
    }
}
