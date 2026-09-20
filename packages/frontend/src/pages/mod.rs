//! One module per page. `render` picks the one a route belongs to.

pub mod home;
pub mod parts;
pub mod strategies;

use dominator::{Dom, html};

use crate::prerender;
use crate::route::Route;

/// The page for a route.
pub fn render(route: &Route) -> Dom {
    match route {
        Route::Home => home::render(),
        Route::Strategies => strategies::render(),
        // Pages land here one at a time as they're built. Until a route has
        // its interactive page, the app renders the same markup the build
        // pre-renders for it — the same data, from the same modules, without
        // the interaction.
        other => static_page(other),
    }
}

/// The pre-rendered view of a route, rendered by the app.
fn static_page(route: &Route) -> Dom {
    html!("div", {
        .class("stack-lg")
        .prop("innerHTML", prerender::page_html(route))
    })
}
