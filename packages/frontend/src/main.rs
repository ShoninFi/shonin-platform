//! The Shonin web app. Routes and copy live in the library (`src/route.rs`),
//! shared with the build-time SEO tool (`src/bin/seo.rs`).

use dominator::{Dom, html, link, routing};
use frontend::route::Route;
use futures_signals::signal::SignalExt;

fn main() {
    console_error_panic_hook::set_once();

    // The build writes each route's copy into #prerender for crawlers and
    // first paint. The app renders the same content live, so replace it.
    if let Some(prerender) = document().get_element_by_id("prerender") {
        prerender.remove();
    }
    dominator::append_dom(&dominator::body(), app());
}

fn app() -> Dom {
    html!("main", {
        .children_signal_vec(routing::url().signal_ref(|url| {
            let route = Route::from_path(&path_of(url));
            document().set_title(route.page().title);
            page(route)
        }).to_signal_vec())
    })
}

/// The same markup the SEO tool pre-renders, so nothing shifts when the app boots.
fn page(route: Route) -> Vec<Dom> {
    let page = route.page();

    let mut children = vec![
        link!("/", {
            .attr("aria-label", "Shonin home")
            .child(html!("img", {
                .class("logo")
                .attr("src", "/public/logo.svg")
                .attr("alt", "")
            }))
        }),
        html!("h1", { .text(page.heading) }),
    ];
    children.extend(
        page.body
            .iter()
            .map(|paragraph| html!("p", { .text(paragraph) })),
    );
    if let Some(note) = page.note {
        children.push(html!("span", {
            .class("soon")
            .text(note)
        }));
    }
    children.push(html!("nav", {
        .children(Route::INDEXABLE.iter().map(|&target| {
            let path = target.path().expect("indexable routes have a path");
            link!(path, {
                .apply_if(target == route, |dom| dom.attr("aria-current", "page"))
                .text(target.label())
            })
        }))
    }));

    children
}

fn path_of(url: &str) -> String {
    web_sys::Url::new(url)
        .map(|url| url.pathname())
        .unwrap_or_default()
}

fn document() -> web_sys::Document {
    web_sys::window()
        .and_then(|window| window.document())
        .expect("running in a browser")
}
