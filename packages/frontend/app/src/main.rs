//! The Shonin web app, in the browser.

use app::atoms::{dialog, toast};
use app::route::Route;
use app::{api, clock, components, shell, web};
use futures_signals::signal::SignalExt;

fn main() {
    console_error_panic_hook::set_once();

    // The build wrote each route's page into #prerender, so there was real
    // content before any of this ran. The app draws the same page, live.
    let document = web::document().expect("running in a browser");
    if let Some(prerender) = document.get_element_by_id("prerender") {
        prerender.remove();
    }

    clock::start();
    api::follow();
    // Wallets: discovery, the vault chains' configs, a silent reconnect.
    app::wallet::get().start();
    app::wallet::solana::get().start();

    let strip_dismissed = components::strip::dismissed();
    let first = std::cell::Cell::new(true);
    let pages = dominator::routing::url().signal_cloned().map(move |url| {
        let (path, query) = path_and_query(&url);
        let mut route = Route::from_url(&path, &query);
        if route == Route::Home {
            // The root is the leaderboard; say so in the address bar too.
            if let Some(history) = web::window().and_then(|window| window.history().ok()) {
                let _ = history.replace_state_with_url(
                    &wasm_bindgen::JsValue::NULL,
                    "",
                    Some("/agents"),
                );
            }
            route = Route::Agents { author: None };
        }
        if let Some(document) = web::document() {
            document.set_title(&route.page().title);
        }
        if dialog::is_open() {
            dialog::close_for_navigation();
        }
        // Following a link starts at the top of the new page, with focus on
        // its content; the first page leaves focus where the browser puts it.
        if !first.replace(false) {
            if let Some(window) = web::window() {
                window.scroll_to_with_x_and_y(0.0, 0.0);
            }
            focus_content();
        }
        Some(shell::shell(&route, strip_dismissed).into_dom())
    });

    let body = dominator::body();
    dominator::append_dom(
        &body,
        dominator::html!("div", {
            .class("sh-contents")
            .child_signal(pages)
        }),
    );
    dominator::append_dom(&body, dialog::layer().into_dom());
    dominator::append_dom(&body, toast::region().into_dom());
}

fn focus_content() {
    wasm_bindgen_futures::spawn_local(async {
        // After the new page is in place.
        gloo_timers::future::TimeoutFuture::new(0).await;
        if let Some(main) = web::document()
            .and_then(|document| document.get_element_by_id("content"))
            .and_then(|main| wasm_bindgen::JsCast::dyn_into::<web_sys::HtmlElement>(main).ok())
        {
            web::focus(&main);
        }
    });
}

fn path_and_query(url: &str) -> (String, String) {
    web_sys::Url::new(url)
        .map(|url| (url.pathname(), url.search()))
        .unwrap_or_default()
}
