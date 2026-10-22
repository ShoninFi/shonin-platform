//! Dialog and sheet: a decision that has to be made before something
//! happens. A dialog on desktop and tablet, a bottom sheet under 600px, the
//! same content. `aria-modal`, labelled by its title; focus moves in, is
//! trapped, and returns to whatever opened it. Esc, the close button and a
//! click on the scrim close it; a sheet also closes when swiped down past
//! 30% of its height or flicked. The page behind is inert.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use futures_signals::signal::{Mutable, SignalExt};
use ui::icons::Icon;
use wasm_bindgen::JsCast;

use super::button::icon_button;
use crate::view::{El, View, el};

/// What a dialog says.
pub struct Dialog {
    pub title: String,
    pub body: View,
    /// The buttons along the bottom: the safe one first, the action last.
    pub foot: Option<View>,
    pub wide: bool,
    /// An alert (`alertdialog`): this chip above the title, and no close
    /// button; Esc, the scrim and a swipe still close it. Its body's message
    /// carries [`DESCRIPTION_ID`], which describes the dialog.
    pub alert: Option<View>,
}

/// The id of an alert dialog's message, which describes it
/// (`aria-describedby`).
pub const DESCRIPTION_ID: &str = "sh-dialog-desc";

type Slot = Rc<RefCell<Option<Dialog>>>;

thread_local! {
    static OPEN: Mutable<Option<Slot>> = Mutable::new(None);
    static RETURN_TO: RefCell<Option<web_sys::HtmlElement>> = const { RefCell::new(None) };
    static LOCKED: Cell<bool> = const { Cell::new(false) };
}

/// Keep the open dialog or sheet from closing (a wallet is being asked to
/// sign): Esc, the close button, the scrim and a swipe do nothing until it
/// is unlocked.
pub fn lock(locked: bool) {
    LOCKED.with(|cell| cell.set(locked));
}

/// Open a dialog over the page, remembering what had focus.
pub fn open(dialog: Dialog) {
    RETURN_TO.with(|slot| *slot.borrow_mut() = crate::web::focused());
    OPEN.with(|open| open.set(Some(Rc::new(RefCell::new(Some(dialog))))));
    crate::web::set_inert(true);
}

/// Close the open dialog whatever its lock: the page under it is going
/// away (a route change). A flow it was running carries on.
pub fn close_for_navigation() {
    lock(false);
    close();
}

/// Close the open dialog and give focus back.
pub fn close() {
    if LOCKED.with(Cell::get) {
        return;
    }
    let was_open = OPEN.with(|open| open.replace(None).is_some());
    if !was_open {
        return;
    }
    crate::web::set_inert(false);
    if let Some(element) = RETURN_TO.with(|slot| slot.borrow_mut().take()) {
        crate::web::focus(&element);
    }
}

pub fn is_open() -> bool {
    OPEN.with(|open| open.lock_ref().is_some())
}

/// Where dialogs appear: once per page, outside the app's own content so
/// that content can be made inert.
pub fn layer() -> El {
    let open = OPEN.with(Clone::clone);
    el("div").class("sh-contents").child_signal(
        None,
        open.signal_cloned().map(|slot| {
            let dialog = slot?.borrow_mut().take()?;
            Some(render(dialog))
        }),
    )
}

fn render(dialog: Dialog) -> View {
    const TITLE_ID: &str = "sh-dialog-title";
    let alert = dialog.alert.is_some();
    let title = el("h2")
        .class("sh-dialog__title")
        .attr("id", TITLE_ID)
        .attr("tabindex", "-1")
        .text(dialog.title);
    let head = el("div")
        .class("sh-dialog__head")
        .class_if(alert, "sh-dialog__head--alert")
        .child(
            el("span")
                .class("sh-dialog__grab")
                .attr("aria-hidden", "true"),
        );
    let head = match dialog.alert {
        Some(chip) => head.child(chip).child(title),
        None => head.child(title).child(
            icon_button(Icon::Close, "Close dialog")
                .class("sh-dialog__close")
                .hook(|dom| dom.event(|_: dominator::events::Click| close())),
        ),
    };
    let mut panel = el("div")
        .class("sh-dialog")
        .class_if(dialog.wide, "sh-dialog--wide")
        .attr("role", if alert { "alertdialog" } else { "dialog" })
        .attr("aria-modal", "true")
        .attr("aria-labelledby", TITLE_ID)
        .attr_if(alert, "aria-describedby", DESCRIPTION_ID)
        .child(head)
        .child(el("div").class("sh-dialog__body").child(dialog.body));
    if let Some(foot) = dialog.foot {
        panel = panel.child(el("div").class("sh-dialog__foot").child(foot));
    }
    let panel = panel.hook(|dom| {
        let element = dom.__internal_element();
        let swipe = element.clone();
        let target = element.clone();
        // Once the body's bound content has drawn (it arrives a tick after
        // the dialog is inserted), so focus lands on what the reader sees first.
        dom.future(async move {
            gloo_timers::future::TimeoutFuture::new(0).await;
            focus_first(&target);
        })
        // Not passive, so the trap's `prevent_default` keeps the browser
        // from also moving focus on.
        .event_with_options(
            &dominator::EventOptions::preventable(),
            move |event: dominator::events::KeyDown| trap(&element, event),
        )
        .apply(|dom| swipe_to_close(dom, swipe))
    });
    el("div")
        .class("sh-scrim")
        .child(panel)
        .hook(|dom| {
            dom.event(|event: dominator::events::Click| {
                // Only a click on the scrim itself, not one inside the panel.
                let on_scrim = event
                    .dyn_target::<web_sys::Element>()
                    .is_some_and(|target| target.class_list().contains("sh-scrim"));
                if on_scrim {
                    close();
                }
            })
            .global_event(|event: dominator::events::KeyDown| {
                if event.key() == "Escape" {
                    close();
                }
            })
        })
        .into()
}

/// Focus the dialog's first control in the body or the buttons (never the
/// close button, and a tab row at its selected tab), else its title.
fn focus_first(element: &web_sys::Element) {
    let first = crate::web::focusables(element)
        .into_iter()
        .find(|control| {
            !control.class_list().contains("sh-dialog__close")
                && !(control.get_attribute("role").as_deref() == Some("tab")
                    && control.get_attribute("aria-selected").as_deref() == Some("false"))
        })
        .or_else(|| {
            element
                .query_selector(".sh-dialog__title")
                .ok()
                .flatten()
                .and_then(|title| title.dyn_into::<web_sys::HtmlElement>().ok())
        });
    if let Some(first) = first {
        crate::web::focus(&first);
    }
}

/// Which end of the dialog's controls Tab moves focus to instead of the
/// browser's next: past the last (or from outside) to the first, and with
/// Shift before the first (or from outside) to the last.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Wrap {
    First,
    Last,
}

fn wrap(shift: bool, at_first: bool, at_last: bool, inside: bool) -> Option<Wrap> {
    if shift && (at_first || !inside) {
        Some(Wrap::Last)
    } else if !shift && (at_last || !inside) {
        Some(Wrap::First)
    } else {
        None
    }
}

/// Keep Tab and Shift+Tab inside the dialog.
fn trap(element: &web_sys::Element, event: dominator::events::KeyDown) {
    if event.key() != "Tab" {
        return;
    }
    let controls = crate::web::focusables(element);
    let (Some(first), Some(last)) = (controls.first(), controls.last()) else {
        return;
    };
    let active = crate::web::focused();
    let at = |control: &web_sys::HtmlElement| {
        active
            .as_ref()
            .is_some_and(|active| active.is_same_node(Some(control)))
    };
    let inside = active
        .as_ref()
        .is_some_and(|active| element.contains(Some(active)));
    let to = match wrap(event.shift_key(), at(first), at(last), inside) {
        Some(Wrap::First) => first,
        Some(Wrap::Last) => last,
        None => return,
    };
    event.prevent_default();
    crate::web::focus(to);
}

/// On a phone the dialog is a sheet: dragging its top edge down moves it
/// with the finger, and letting go past 30% of its height (or with a flick)
/// closes it; otherwise it settles back.
fn swipe_to_close(
    dom: dominator::DomBuilder<web_sys::Element>,
    element: web_sys::Element,
) -> dominator::DomBuilder<web_sys::Element> {
    let start: Rc<Cell<Option<(f64, f64)>>> = Rc::new(Cell::new(None));
    let moved = Rc::new(Cell::new(0.0));
    let (down_start, move_start, up_start) = (start.clone(), start.clone(), start);
    let (move_moved, up_moved) = (moved.clone(), moved);
    let (down_el, move_el, up_el) = (element.clone(), element.clone(), element);
    let style = |element: &web_sys::Element, value: &str| {
        if let Some(element) = element.dyn_ref::<web_sys::HtmlElement>() {
            let _ = element.style().set_property("transform", value);
        }
    };
    dom.event(move |event: dominator::events::PointerDown| {
        if crate::web::width() >= 600.0 {
            return;
        }
        let top = down_el.get_bounding_client_rect().top();
        // Only from the grabber and title row, so the content still scrolls.
        if f64::from(event.y()) - top > 64.0 {
            return;
        }
        down_start.set(Some((f64::from(event.y()), crate::web::now_ms())));
        let _ = down_el.set_attribute("data-dragging", "");
    })
    .global_event(move |event: dominator::events::PointerMove| {
        let Some((from, _)) = move_start.get() else {
            return;
        };
        let by = (f64::from(event.y()) - from).max(0.0);
        move_moved.set(by);
        style(&move_el, &format!("translateY({by}px)"));
    })
    .global_event(move |_: dominator::events::PointerUp| {
        let Some((_, at)) = up_start.take() else {
            return;
        };
        let by = up_moved.replace(0.0);
        let height = up_el.get_bounding_client_rect().height().max(1.0);
        let speed = by / (crate::web::now_ms() - at).max(1.0);
        let _ = up_el.remove_attribute("data-dragging");
        if by > height * 0.3 || speed > 0.6 {
            close();
        } else {
            let _ = up_el.set_attribute("data-settling", "");
            style(&up_el, "translateY(0)");
        }
    })
}

#[cfg(test)]
mod tests {
    use super::{Wrap, wrap};

    #[test]
    fn an_alert_dialog_is_described_by_its_message_and_a_plain_dialog_is_not() {
        use crate::view::{View, el};
        let dialog = |alert: bool| super::Dialog {
            title: "Switch network".to_string(),
            body: el("p").attr("id", super::DESCRIPTION_ID).text("Why").into(),
            foot: None,
            wide: false,
            alert: alert.then(|| el("span").text("Wrong network").into()),
        };
        let alert = View::Many(vec![super::render(dialog(true))]).html();
        assert!(alert.contains("role=\"alertdialog\""), "{alert}");
        assert!(
            alert.contains(&format!("aria-describedby=\"{}\"", super::DESCRIPTION_ID)),
            "{alert}"
        );
        let plain = View::Many(vec![super::render(dialog(false))]).html();
        assert!(!plain.contains("aria-describedby"), "{plain}");
    }

    #[test]
    fn tab_wraps_from_the_last_control_to_the_first_and_shift_tab_from_the_first_to_the_last() {
        // (shift, at first, at last, focus inside)
        assert_eq!(wrap(false, false, true, true), Some(Wrap::First));
        assert_eq!(wrap(true, true, false, true), Some(Wrap::Last));
        assert_eq!(wrap(false, false, false, false), Some(Wrap::First));
        assert_eq!(wrap(true, false, false, false), Some(Wrap::Last));
        // Between the ends the browser moves focus on by itself.
        assert_eq!(wrap(false, true, false, true), None);
        assert_eq!(wrap(true, false, true, true), None);
        assert_eq!(wrap(false, false, false, true), None);
    }
}
