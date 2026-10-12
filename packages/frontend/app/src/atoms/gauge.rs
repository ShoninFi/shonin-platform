//! Tank gauge: how long an agent can keep running. The fill is days left over
//! 60 days, gain at 7 days or more and caution under 7; the runway is always
//! in words beside it, and `aria-valuetext` carries the sentence.

use futures_signals::signal::Mutable;

use crate::format;
use crate::model::{GAUGE_DAYS, TankState};
use crate::view::{El, svg};

/// A gauge bound to the tank. `mini` is the 48 x 6 one in a table row.
pub fn gauge(tank: &Mutable<TankState>, mini: bool) -> El {
    svg("svg")
        .class("sh-gauge")
        .class_if(mini, "sh-gauge--mini")
        .class_from("sh-gauge--low", tank, |tank| tank.is_low())
        .class_from("sh-gauge--empty", tank, |tank| tank.is_empty())
        .attr("role", "meter")
        .attr("aria-label", "Tank")
        .attr("aria-valuemin", "0")
        .attr("aria-valuemax", format::number(GAUGE_DAYS, 0))
        .attr_from("aria-valuenow", tank, |tank| {
            Some(format::number((tank.fill() * GAUGE_DAYS).floor(), 0))
        })
        .attr_from("aria-valuetext", tank, |tank| Some(tank.sentence()))
        .attr("preserveAspectRatio", "none")
        .child(
            svg("rect")
                .class("sh-gauge__track")
                .attr("width", "100%")
                .attr("height", "100%")
                .attr("rx", "3"),
        )
        .child(
            svg("rect")
                .class("sh-gauge__fill")
                .attr("height", "100%")
                .attr("rx", "3")
                .attr_from("width", tank, |tank| {
                    Some(format!("{:.0}%", (tank.fill() * 100.0).max(4.0)))
                }),
        )
}
