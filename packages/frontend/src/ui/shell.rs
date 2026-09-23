//! The frame every page sits in: the masthead, the navigation and the footer.

use dominator::{Dom, events, html, link};
use futures_signals::signal::SignalExt;

use crate::copy;
use crate::route::{HANDBOOK, NavItem, Route};
use crate::state::{self, Theme};
use crate::ui::icon;

/// A link straight to the page, so a keyboard doesn't have to tab through the
/// navigation on every route.
pub fn skip_link() -> Dom {
    html!("a", {
        .class("skip-link")
        .attr("href", "#main")
        .text("Skip to the page")
    })
}

/// The header: the mark, the primary navigation, and the theme control.
pub fn masthead(route: &Route) -> Dom {
    let section = route.section();

    html!("header", {
        .class("masthead")
        .child(html!("div", {
            .class(["page", "masthead__inner"])
            .child(link!("/", {
                .class("brand")
                .attr("aria-label", "Shonin home")
                .child(html!("img", {
                    .class("brand__mark")
                    .attr("src", "/public/logo.svg")
                    .attr("alt", "")
                }))
                .child(html!("span", { .class("brand__word").text("Shonin") }))
            }))
            .child(html!("nav", {
                .class("mainnav")
                .attr("aria-label", "Primary")
                .children(Route::nav().into_iter().map(|item| {
                    let current = item.is_current(section.as_ref());
                    match item {
                        NavItem::Page { route, label } => {
                            let path = route.path().unwrap_or_else(|| "/".to_string());
                            link!(path, {
                                .class("mainnav__link")
                                .apply_if(current, |dom| dom.attr("aria-current", "page"))
                                .text(label)
                            })
                        }
                        NavItem::Site { href, label } => {
                            offsite(href, label, Some("mainnav__link"))
                        }
                    }
                }))
            }))
            .child(html!("div", {
                .class("masthead__tools")
                .child(crate::pages::invest::wallet_button())
                .child(theme_button())
            }))
        }))
    })
}

fn theme_button() -> Dom {
    html!("button", {
        .class(["btn", "btn--quiet"])
        .attr("type", "button")
        .attr_signal("aria-label", state::get().theme.signal().map(|theme| {
            match theme {
                Theme::Dark => "Switch to the light theme",
                Theme::Light => "Switch to the dark theme",
            }
        }))
        .child_signal(state::get().theme.signal().map(|theme| {
            Some(match theme {
                Theme::Dark => icon::sun(),
                Theme::Light => icon::moon(),
            })
        }))
        .event(|_: events::Click| state::toggle_theme())
    })
}

/// The footer.
pub fn colophon() -> Dom {
    html!("footer", {
        .class("colophon")
        .child(html!("div", {
            .class(["page", "colophon__inner"])
            .child(html!("div", {
                .class("colophon__links")
                .children([
                    ("/about", "About"),
                    ("/verify", "Verify"),
                    ("/status", "Status"),
                    ("/publish", "Publish an agent"),
                ].into_iter().map(|(path, label)| link!(path, { .text(label) })))
                .child(offsite(HANDBOOK, "Docs", None))
            }))
            .child(html!("div", { .class("faint").text(copy::PITCH) }))
        }))
    })
}

/// A link off this site. The router only knows this app's own paths, so an
/// outward link has to be a plain anchor rather than one it would intercept.
pub fn offsite(href: &str, label: &str, class: Option<&str>) -> Dom {
    html!("a", {
        .apply(|dom| match class {
            Some(name) => dom.class(name),
            None => dom,
        })
        .attr("href", href)
        .text(label)
    })
}

/// A link that reads as a button.
pub fn button_link(path: &str, label: &str, primary: bool) -> Dom {
    link!(path.to_string(), {
        .class("btn")
        .apply_if(primary, |dom| dom.class("btn--primary"))
        .text(label)
    })
}

/// A button that runs something, with the app's button styling.
pub fn button(label: &str, primary: bool, mut on_click: impl FnMut() + 'static) -> Dom {
    html!("button", {
        .class("btn")
        .apply_if(primary, |dom| dom.class("btn--primary"))
        .attr("type", "button")
        .text(label)
        .event(move |_: events::Click| on_click())
    })
}
