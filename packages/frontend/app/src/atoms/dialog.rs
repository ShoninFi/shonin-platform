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
}

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
    let head = el("div")
        .class("sh-dialog__head")
        .child(
            el("span")
                .class("sh-dialog__grab")
                .attr("aria-hidden", "true"),
        )
        .child(
            el("h2")
                .class("sh-dialog__title")
                .attr("id", TITLE_ID)
                .attr("tabindex", "-1")
                .text(dialog.title),
        )
        .child(
            icon_button(Icon::Close, "Close dialog")
                .class("sh-dialog__close")
                .hook(|dom| dom.event(|_: dominator::events::Click| close())),
        );
    let mut panel = el("div")
        .class("sh-dialog")
        .class_if(dialog.wide, "sh-dialog--wide")
        .attr("role", "dialog")
        .attr("aria-modal", "true")
        .attr("aria-labelledby", TITLE_ID)
        .child(head)
        .child(el("div").class("sh-dialog__body").child(dialog.body));
    if let Some(foot) = dialog.foot {
        panel = panel.child(el("div").class("sh-dialog__foot").child(foot));
    }
    let panel = panel.hook(|dom| {
        let element = dom.__internal_element();
        let swipe = element.clone();
        dom.after_inserted(|element: web_sys::Element| {
            // The first control in the body or the buttons, else the title.
            let first = crate::web::focusables(&element)
                .into_iter()
                .find(|control| !control.class_list().contains("sh-dialog__close"))
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
        })
        .event(move |event: dominator::events::KeyDown| trap(&element, event))
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
    if event.shift_key() && (at(first) || !inside) {
        event.prevent_default();
        crate::web::focus(last);
    } else if !event.shift_key() && (at(last) || !inside) {
        event.prevent_default();
        crate::web::focus(first);
    }
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
