//! Skeleton: a shimmering shape exactly where data will land, so nothing
//! jumps. No spinners. The region it stands in carries `aria-busy="true"`.
//! Drawn as an SVG box so its size needs no inline style.

use crate::view::{El, svg};

/// A skeleton `width` wide (pixels or a percentage) and `height` pixels tall.
pub fn skeleton(width: &'static str, height: u32) -> El {
    svg("svg")
        .class("sh-skel")
        .attr("width", width)
        .attr("height", height.to_string())
        .attr("aria-hidden", "true")
        .attr("focusable", "false")
}

/// A skeleton with the rounder corners of a larger shape.
pub fn skeleton_block(width: &'static str, height: u32) -> El {
    skeleton(width, height).class("sh-skel--block")
}
