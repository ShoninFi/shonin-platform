//! The app header: 64px on tablet and desktop, sticky, with the lockup (to
//! shonin.fi), the sections and Docs, and the wallet control on the right.
//! Below desktop the header tightens so the wallet control never meets
//! the sections (see `header.css`). Under 600px it is the 56px phone header: the lockup on /agents, else the
//! mark and the page's title; Docs moves to the footer.

use ui::icons::Icon;

use crate::atoms::icon;
use crate::atoms::logo::{Logo, home};
use crate::route::{DOCS, Route, Section};
use crate::view::{View, el};
use crate::web::app_link;

pub fn header(route: &Route) -> View {
    let current = route.section();
    let title = route.phone_title();
    let nav = el("nav")
        .class("sh-appheader__nav")
        .attr("aria-label", "App")
        .children(Section::ALL.iter().map(|section| {
            app_link(section.href())
                .class("sh-nav")
                .attr_if(current == Some(*section), "aria-current", "page")
                .text(section.label())
        }))
        .child(
            el("a")
                .class("sh-nav")
                .class("sh-nav--offsite")
                .attr("href", DOCS)
                .child(el("span").class("sh-nav__label").text("Docs"))
                .child(icon(Icon::External, 16))
                .child(el("span").class("sh-vh").text("(opens the docs site)")),
        );
    let inner = el("div")
        .class("sh-container")
        .class("sh-appheader__inner")
        .child(home(Logo::Lockup))
        .child(home(Logo::Mark));
    let inner = match &title {
        Some(title) => inner.child(
            el("p")
                .class("sh-appheader__title")
                .attr("aria-hidden", "true")
                .text(title.clone()),
        ),
        None => inner,
    };
    el("header")
        .class("sh-appheader")
        .class_if(title.is_some(), "sh-appheader--titled")
        .child(
            inner.child(nav).child(
                el("div")
                    .class("sh-appheader__tools")
                    .child(super::wallet::header_control(route)),
            ),
        )
        .into()
}
