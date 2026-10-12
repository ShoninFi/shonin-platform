//! The app header: 64px on tablet and desktop, sticky, with the lockup (to
//! shonin.fi), the sections and Docs, and the wallet control on the right.
//! Under 600px it is the 56px phone header: the lockup on /agents, else the
//! mark and the page's title; Docs moves to the footer.

use ui::icons::Icon;

use crate::atoms::button::{Button, Kind, Size};
use crate::atoms::dialog::{self, Dialog};
use crate::atoms::icon;
use crate::atoms::logo::{Logo, home};
use crate::atoms::notice::{NoticeTone, notice};
use crate::route::{DOCS, Route, Section};
use crate::view::{View, el, text};
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
                .text("Docs")
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
                    .child(connect_button()),
            ),
        )
        .into()
}

/// "Connect wallet" ("Connect" on phone). The wallet arrives in a later
/// update; until then it says so, plainly, in a dialog.
fn connect_button() -> crate::view::El {
    let label = View::Many(vec![
        el("span")
            .class("sh-appheader__long")
            .text("Connect wallet")
            .into(),
        el("span")
            .class("sh-appheader__short")
            .text("Connect")
            .into(),
    ]);
    crate::atoms::button::on_click(
        Button::new(Kind::Primary, label)
            .size(Size::Small)
            .build()
            .attr("aria-haspopup", "dialog"),
        || {
            dialog::open(Dialog {
                title: "Connect wallet".to_string(),
                body: View::Many(vec![
                    el("p")
                        .text("Shonin never holds your money or your keys. Connecting lets you see your shares and send deposits and withdrawals yourself.")
                        .into(),
                    notice(
                        NoticeTone::Info,
                        text("Wallet connections arrive in the next update of this app. Every page reads without one."),
                    )
                    .into(),
                ]),
                foot: Some(
                    crate::atoms::button::on_click(
                        Button::new(Kind::Secondary, "Close").build(),
                        dialog::close,
                    )
                    .into(),
                ),
                wide: false,
            });
        },
    )
}
