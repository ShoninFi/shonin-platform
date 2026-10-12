//! Sparkline: the shape of the last 30 days beside its number. 96 x 28,
//! a 1.5 ink-muted stroke, no fill, dots or axis, never gain or loss colour.
//! Hidden from screen readers: the number beside it says it.

use futures_signals::signal::Mutable;

use crate::model::spark_path;
use crate::view::{El, svg};

const WIDTH: f64 = 96.0;
const HEIGHT: f64 = 28.0;
/// A level line, dashed, for an agent with too little history to draw.
const FLAT: &str = "M0 14L96 14";

/// A sparkline that redraws when its values change.
pub fn sparkline(values: &Mutable<Vec<f64>>) -> El {
    svg("svg")
        .class("sh-spark")
        .class_from("sh-spark--empty", values, |values| {
            spark_path(values, WIDTH, HEIGHT).is_none()
        })
        .attr("width", "96")
        .attr("height", "28")
        .attr("viewBox", "0 0 96 28")
        .attr("preserveAspectRatio", "none")
        .attr("aria-hidden", "true")
        .attr("focusable", "false")
        .child(svg("path").attr_from("d", values, |values| {
            Some(spark_path(values, WIDTH, HEIGHT).unwrap_or_else(|| FLAT.to_string()))
        }))
}
