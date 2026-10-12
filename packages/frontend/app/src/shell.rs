//! The frame every page sits in: the skip link, the header, the freshness
//! bar, the page, the footer and the phone tab bar.

use crate::components::{footer, freshness, header, tabbar};
use crate::pages;
use crate::route::Route;
use crate::view::{El, View, el};

/// The whole page for a route, as one element: `#app`.
pub fn shell(route: &Route, strip_dismissed: bool) -> El {
    let page = pages::page(route, strip_dismissed);
    let full_bleed = matches!(route, Route::NotFound)
        || (matches!(route, Route::Atoms) && !cfg!(feature = "dev"));
    let main = el("main")
        .attr("id", "content")
        .attr("tabindex", "-1")
        .class("sh-main");
    let main = if full_bleed {
        main.child(page.body)
    } else {
        main.child(
            el("div")
                .class("sh-container")
                .class("sh-page")
                .child(freshness::card())
                .child(page.body),
        )
    };
    el("div")
        .attr("id", "app")
        .class("sh-app")
        .class_if(route.has_tab_bar(), "sh-app--tabbar")
        .child(
            el("a")
                .class("sh-skip")
                .attr("href", "#content")
                .text("Skip to content"),
        )
        .child(header::header(route))
        .child(freshness::bar())
        .child(page.top.unwrap_or(View::Empty))
        .child(main)
        .child(if full_bleed {
            View::Empty
        } else {
            footer::footer(route)
        })
        .child(tabbar::tabbar(route))
}
