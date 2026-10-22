//! The frame every page sits in: the skip link, the test-network line, the
//! header, the freshness bar, the page, the footer and the phone tab bar.

use crate::components::{footer, freshness, header, tabbar, testnet};
use crate::pages;
use crate::route::Route;
use crate::view::{El, View, el};

/// The whole page for a route, as one element: `#app`.
pub fn shell(route: &Route, strip_dismissed: bool) -> El {
    frame(route, pages::page(route, strip_dismissed))
}

/// The frame around `page`, as it sits under `route`.
pub fn frame(route: &Route, page: pages::Page) -> El {
    let full_bleed = matches!(route, Route::NotFound)
        || (matches!(route, Route::Atoms | Route::States) && !cfg!(feature = "dev"));
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
                .child(freshness::card(!matches!(route, Route::Status)))
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
        .child(testnet::line())
        .child(header::header(route))
        .child(crate::components::wallet::page_wrong_network_bar(route))
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
