//! The Shonin web app. Routes, data and copy live in the library, shared with
//! the build-time pre-render (`src/bin/seo.rs`).

use app::{
    api, data, pages,
    route::Route,
    state,
    ui::{overlay, shell},
};
use dominator::{Dom, html, routing};
use futures_signals::map_ref;
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
    // Everything on the screen comes from here, and keeps coming: the app
    // asks the platform for a newer snapshot every couple of seconds and
    // re-renders when it gets one.
    api::follow();
    dominator::append_dom(&dominator::body(), app());
}

fn app() -> Dom {
    let address = routing::url().signal_cloned();
    let snapshots = data::changes();
    let reaching_it = data::loading();

    // The page is rendered again when the viewer navigates, when a newer
    // snapshot arrives, or when the app's view of the platform changes.
    let pages = map_ref! {
        let url = address,
        let version = snapshots,
        let loading = reaching_it => (url.clone(), loading.clone(), *version)
    }
    .map(|(url, loading, _version)| {
        let route = Route::from_path(&path_of(&url));
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
                    .child(match (&loading, data::current().is_some()) {
                        // Nothing has arrived yet, so there is nothing honest
                        // to render.
                        (data::Loading::Waiting, false) => reaching(),
                        (data::Loading::Failed(why), false) => unreachable(why),
                        _ => pages::render(&route),
                    })
                }))
            }),
            shell::colophon(),
            overlay::toast_host(&state::get().toasts),
        ]
    })
    .to_signal_vec();

    html!("div", {
        .class("app")
        .children_signal_vec(pages)
    })
}

/// Before the first snapshot lands.
fn reaching() -> Dom {
    app::ui::primitives::empty("Reading the chain…")
}

/// When the platform cannot be reached at all. The app says what it tried,
/// because a page that is simply blank tells a reader nothing.
fn unreachable(why: &str) -> Dom {
    app::ui::primitives::notice(
        app::ui::primitives::Tone::Warn,
        "The platform is not answering",
        &format!(
            "{why}. Nothing on this page would be true, so none of it is shown. It appears as \
             soon as the platform answers."
        ),
    )
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
