//! The 404 (NotFound-Desktop): the navy hero with an open hexagonal frame,
//! a piece missing, and two ways back. A real 404 status: the static host
//! serves this page for any path it has no file for.

use ui::icons::Icon;

use crate::atoms::button::{Button, Kind, Size};
use crate::components::footer;
use crate::route::SITE;
use crate::view::{El, el, svg};
use crate::web::app_link;

use super::Page;

const FRAME: &str = "M259.4 121.9 Q280.0 110.0 300.6 121.9 L406.6 183.1 Q427.2 195.0 427.2 218.8 L427.2 341.2 Q427.2 365.0 406.6 376.9 L300.6 438.1 Q280.0 450.0 259.4 438.1 L153.4 376.9 Q132.8 365.0 132.8 341.2 L132.8 218.8 Q132.8 195.0 153.4 183.1 Z";
const CORE: &str = "M264.5 161.0 Q280.0 152.0 295.5 161.0 L375.3 207.0 Q390.9 216.0 390.9 233.9 L390.9 326.1 Q390.9 344.0 375.3 353.0 L295.5 399.0 Q280.0 408.0 264.5 399.0 L184.7 353.0 Q169.1 344.0 169.1 326.1 L169.1 233.9 Q169.1 216.0 184.7 207.0 Z";
const OPEN: &str = "M355.4 195.5 L375.3 207.0 Q390.9 216.0 390.9 233.9 L390.9 326.1 Q390.9 344.0 375.3 353.0 L295.5 399.0 Q280.0 408.0 264.5 399.0 L184.7 353.0 Q169.1 344.0 169.1 326.1 L169.1 233.9 Q169.1 216.0 184.7 207.0 L264.5 161.0 Q280.0 152.0 295.5 161.0 L315.5 172.5";
const PIECE: &str = "M347.1 139.9 L367.9 151.9";

fn art() -> El {
    svg("svg")
        .class("nf-art")
        .attr("viewBox", "0 0 560 560")
        .attr("role", "img")
        .attr("aria-label", "An open hexagonal frame with a piece missing; a light runs around it and stops at the gap")
        .child(
            svg("defs").child(
                svg("radialGradient")
                    .attr("id", "nf-core")
                    .attr("cx", "40%")
                    .attr("cy", "35%")
                    .attr("r", "75%")
                    .child(svg("stop").class("nf-core-a").attr("offset", "0"))
                    .child(svg("stop").class("nf-core-b").attr("offset", "1")),
            ),
        )
        .child(
            svg("g")
                .class("nf-orbit")
                .child(svg("circle").class("nf-ring").attr("cx", "280").attr("cy", "280").attr("r", "236"))
                .child(svg("circle").class("nf-moon").attr("cx", "516").attr("cy", "280").attr("r", "4"))
                .child(svg("circle").class("nf-moon-2").attr("cx", "113").attr("cy", "447").attr("r", "3")),
        )
        .child(svg("path").class("nf-frame").class("sh-fade-in").attr("d", FRAME))
        .child(svg("path").class("nf-core").class("sh-fade-in").attr("fill", "url(#nf-core)").attr("d", CORE))
        .child(
            svg("path")
                .class("nf-open")
                .class("sh-draw")
                .attr("d", OPEN)
                .attr("pathLength", "1"),
        )
        .child(svg("path").class("nf-trace").attr("d", OPEN).attr("pathLength", "1"))
        .child(svg("circle").class("nf-pulse").attr("cx", "315.5").attr("cy", "172.5").attr("r", "9"))
        .child(
            svg("g").class("nf-piece").child(
                svg("path")
                    .class("nf-piece__line")
                    .attr("d", PIECE)
                    .attr("transform", "rotate(14 357.5 145.9)"),
            ),
        )
        .child(svg("text").class("nf-404").class("sh-fade-in").attr("x", "280").attr("y", "302").attr("text-anchor", "middle").text("404"))
        .child(svg("text").class("nf-note").attr("x", "398").attr("y", "128").text("No page at this address"))
        .child(svg("text").class("nf-note").attr("x", "96").attr("y", "492").text("Published agents never change"))
}

fn grid() -> El {
    svg("svg")
        .class("nf-grid")
        .attr("aria-hidden", "true")
        .attr("focusable", "false")
        .child(
            svg("defs").child(
                svg("pattern")
                    .attr("id", "nf-hexgrid")
                    .attr("width", "56")
                    .attr("height", "97")
                    .attr("patternUnits", "userSpaceOnUse")
                    .child(svg("path").attr(
                        "d",
                        "M28 0 L56 16.2 L56 48.5 L28 64.7 L0 48.5 L0 16.2 Z M28 64.7 L28 97",
                    )),
            ),
        )
        .child(
            svg("rect")
                .attr("width", "100%")
                .attr("height", "100%")
                .attr("fill", "url(#nf-hexgrid)"),
        )
}

pub fn page() -> Page {
    let body = el("section")
        .class("sh-hero")
        .class("nf")
        .attr("aria-labelledby", "nf-title")
        .child(el("div").class("nf-glow").class("sh-glow-drift").attr("aria-hidden", "true"))
        .child(grid())
        .child(
            el("div")
                .class("sh-container")
                .class("nf-inner")
                .child(
                    el("div")
                        .class("nf-copy")
                        .child(el("p").class("sh-label").class("nf-eyebrow").class("sh-reveal").text("404 · Page not found"))
                        .child(
                            el("h1")
                                .attr("id", "nf-title")
                                .class("nf-title")
                                .class("sh-reveal")
                                .text("This page isn’t on the chain, or anywhere else."),
                        )
                        .child(el("p").class("nf-lede").class("sh-reveal").text(
                            "The link may be old, or the agent may have a new page. Published agents never change, so a changed agent has a new address.",
                        ))
                        .child(
                            el("div")
                                .class("nf-actions")
                                .class("sh-reveal")
                                .child(crate::web::internal_link(
                                    Button::new(Kind::Hero, "Browse agents")
                                        .size(Size::Large)
                                        .icon_after(Icon::ArrowRight)
                                        .href("/agents")
                                        .build(),
                                    "/agents".to_string(),
                                ))
                                .child(Button::new(Kind::Hero2, "Go home").size(Size::Large).href(SITE).build()),
                        )
                        .child(
                            el("p")
                                .class("nf-status")
                                .class("sh-reveal")
                                .text("Looking for a vault that was stopped? Every kill and freeze is on the ")
                                .child(app_link("/status").text("status page"))
                                .text("."),
                        ),
                )
                .child(el("div").class("nf-art-wrap").child(art())),
        )
        .child(el("div").class("sh-container").class("nf-foot").child(footer::bottom()));
    Page {
        top: None,
        body: body.into(),
    }
}
