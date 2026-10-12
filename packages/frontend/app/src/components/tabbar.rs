//! The phone tab bar: Agents, Portfolio and Publish, on those three routes
//! only, 64px plus the safe area. Hidden from 600px up.

use ui::icons::Icon;

use crate::atoms::icon;
use crate::route::{Route, Section};
use crate::view::{View, el};
use crate::web::app_link;

pub fn tabbar(route: &Route) -> View {
    if !route.has_tab_bar() {
        return View::Empty;
    }
    let current = route.section();
    el("nav")
        .class("sh-tabbar")
        .attr("aria-label", "Sections")
        .children(Section::ALL.iter().map(|section| {
            let glyph = match section {
                Section::Agents => Icon::Vault,
                Section::Portfolio => Icon::Wallet,
                Section::Publish => Icon::Upload,
            };
            app_link(section.href())
                .attr_if(current == Some(*section), "aria-current", "page")
                .child(icon(glyph, 24))
                .text(section.label())
        }))
        .into()
}
