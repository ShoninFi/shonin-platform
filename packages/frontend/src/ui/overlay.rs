//! Things that sit over the page: a modal for anything that needs a decision,
//! and the toasts that say what happened afterwards.

use std::rc::Rc;

use dominator::{Dom, clone, events, html};
use futures_signals::signal::{Mutable, SignalExt};
use futures_signals::signal_vec::{MutableVec, SignalVecExt};

use crate::ui::icon;
use crate::ui::primitives::Tone;

/// A modal, shown while `open` is true.
///
/// Escape closes it, as does the backdrop and the close button, so it can
/// never trap anyone. The first control inside it takes focus when it opens.
pub fn modal(
    open: &Mutable<bool>,
    title: &'static str,
    wide: bool,
    body: impl Fn() -> Vec<Dom> + 'static,
) -> Dom {
    let open = open.clone();
    let body = Rc::new(body);

    html!("div", {
        .child_signal(open.signal().map(clone!(open, body => move |is_open| {
            if !is_open {
                return None;
            }
            Some(html!("div", {
                .class("overlay")
                .attr("role", "dialog")
                .attr("aria-modal", "true")
                .attr("aria-label", title)
                // A click on the backdrop closes; a click inside must not.
                .event(clone!(open => move |_: events::Click| open.set_neq(false)))
                .global_event(clone!(open => move |event: events::KeyDown| {
                    if event.key() == "Escape" {
                        open.set_neq(false);
                    }
                }))
                .child(html!("div", {
                    .class("modal")
                    .apply_if(wide, |dom| dom.class("modal--wide"))
                    .event(|event: events::Click| event.stop_propagation())
                    .child(html!("div", {
                        .class("modal__head")
                        .child(html!("h2", { .class("modal__title").text(title) }))
                        .child(html!("button", {
                            .class(["btn", "btn--quiet"])
                            .attr("type", "button")
                            .attr("aria-label", "Close")
                            .text("✕")
                            .focused(true)
                            .event(clone!(open => move |_: events::Click| open.set_neq(false)))
                        }))
                    }))
                    .child(html!("div", {
                        .class(["modal__body", "stack"])
                        .children(body())
                    }))
                }))
            }))
        })))
    })
}

/// The buttons along the bottom of a modal.
pub fn modal_actions(children: Vec<Dom>) -> Dom {
    html!("div", { .class("modal__foot").children(children) })
}

/// A short message about something that just happened.
#[derive(Clone)]
pub struct Toast {
    pub id: u32,
    pub message: String,
    pub tone: Tone,
}

/// The toasts on screen. One list, shown by `toast_host` in the app's shell.
pub struct Toasts {
    items: MutableVec<Toast>,
    next_id: Mutable<u32>,
}

impl Default for Toasts {
    fn default() -> Self {
        Self::new()
    }
}

impl Toasts {
    pub fn new() -> Self {
        Self {
            items: MutableVec::new(),
            next_id: Mutable::new(1),
        }
    }

    /// Say something happened. It stays until it is dismissed or until three
    /// newer ones have pushed it out, so nothing important disappears while
    /// it is being read.
    pub fn push(&self, message: impl Into<String>, tone: Tone) {
        let id = self.next_id.get();
        self.next_id.set(id + 1);
        self.items.lock_mut().push_cloned(Toast {
            id,
            message: message.into(),
            tone,
        });
        while self.items.lock_ref().len() > 3 {
            self.items.lock_mut().remove(0);
        }
    }

    pub fn dismiss(&self, id: u32) {
        self.items.lock_mut().retain(|toast| toast.id != id);
    }
}

/// Where the toasts appear. Rendered once, in the shell.
pub fn toast_host(toasts: &'static Toasts) -> Dom {
    html!("div", {
        .class("toasts")
        .attr("role", "status")
        .attr("aria-live", "polite")
        .children_signal_vec(toasts.items.signal_vec_cloned().map(move |toast| {
            let id = toast.id;
            html!("div", {
                .class("toast")
                .apply(|dom| match toast.tone {
                    Tone::Error => dom.class("toast--err"),
                    Tone::Ok => dom.class("toast--ok"),
                    _ => dom,
                })
                .child(match toast.tone {
                    Tone::Error => icon::error(),
                    Tone::Warn => icon::warning(),
                    _ => icon::check(),
                })
                .child(html!("div", { .class("grow").text(&toast.message) }))
                .child(html!("button", {
                    .class(["btn", "btn--quiet", "btn--sm"])
                    .attr("type", "button")
                    .attr("aria-label", "Dismiss")
                    .text("✕")
                    .event(move |_: events::Click| toasts.dismiss(id))
                }))
            })
        }))
    })
}
