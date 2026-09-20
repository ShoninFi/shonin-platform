//! The Shonin web app. Routes, data and copy live in the library, shared with
//! the build-time pre-render (`src/bin/seo.rs`).

use dominator::{Dom, html, routing};
use frontend::{
    pages,
    route::Route,
    state,
    ui::{overlay, shell},
};
use futures_signals::signal::SignalExt;

fn main() {
    console_error_panic_hook::set_once();

    // The build writes each route's content into #prerender, so the page has
    // real content before any of this runs. The app renders the same data,
    // so replace it rather than leaving two copies.
    if let Some(prerender) = document().get_element_by_id("prerender") {
        prerender.remove();
    }

    state::apply_theme(state::get().theme.get());
    dominator::append_dom(&dominator::body(), app());
}

fn app() -> Dom {
    html!("div", {
        .class("app")
        .children_signal_vec(routing::url()
            .signal_ref(|url| {
                let route = Route::from_path(&path_of(url));
                let page = route.page();
                document().set_title(&page.title);
                scroll_to_top();
                vec![
                    shell::skip_link(),
                    shell::masthead(&route),
                    html!("main", {
                        .class("app__main")
                        .attr("id", "main")
                        .child(html!("div", {
                            .class(["page", "stack-lg"])
                            .child(pages::render(&route))
                        }))
                    }),
                    shell::colophon(),
                    overlay::toast_host(&state::get().toasts),
                ]
            })
            .to_signal_vec())
    })
}

/// Following a link should start at the top of the new page, the way loading
/// one does.
fn scroll_to_top() {
    if let Some(window) = web_sys::window() {
        window.scroll_to_with_x_and_y(0.0, 0.0);
    }
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
