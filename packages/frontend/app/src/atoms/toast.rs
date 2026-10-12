//! Toast: confirms something that just happened, and is never the only copy
//! of it. Bottom-right on desktop, under the header on phone; at most three,
//! the oldest leaving first. Each stays 6 seconds (the 3px bar shows the
//! timer), pausing while hovered or focused, and closes with its button or
//! Escape. Errors use `role="alert"` and stay until closed; the rest are
//! announced politely.

use std::cell::Cell;
use std::rc::Rc;

use futures_signals::signal::Mutable;
use futures_signals::signal_vec::MutableVec;
use ui::icons::Icon;

use super::button::icon_button;
use super::icon::icon;
use crate::view::{El, View, el};

/// How long a toast stays, in milliseconds.
pub const STAYS_MS: u32 = 6_000;
/// The most toasts on screen at once.
pub const MOST: usize = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToastKind {
    Success,
    Info,
    Neutral,
    Error,
}

impl ToastKind {
    fn class(self) -> &'static str {
        match self {
            ToastKind::Success => "sh-toast--success",
            ToastKind::Info => "sh-toast--info",
            ToastKind::Neutral => "sh-toast--neutral",
            ToastKind::Error => "sh-toast--error",
        }
    }

    fn icon(self) -> Icon {
        match self {
            ToastKind::Success => Icon::Check,
            ToastKind::Info => Icon::Info,
            ToastKind::Neutral => Icon::Info,
            ToastKind::Error => Icon::Serious,
        }
    }
}

pub struct Toast {
    id: u64,
    pub kind: ToastKind,
    pub title: String,
    pub text: String,
    paused: Mutable<bool>,
}

thread_local! {
    static TOASTS: MutableVec<Rc<Toast>> = MutableVec::new();
    static NEXT: Cell<u64> = const { Cell::new(0) };
}

fn toasts() -> MutableVec<Rc<Toast>> {
    TOASTS.with(Clone::clone)
}

/// Say that something happened.
pub fn push(kind: ToastKind, title: impl Into<String>, text: impl Into<String>) {
    let id = NEXT.with(|next| {
        next.set(next.get() + 1);
        next.get()
    });
    let toast = Rc::new(Toast {
        id,
        kind,
        title: title.into(),
        text: text.into(),
        paused: Mutable::new(false),
    });
    let list = toasts();
    {
        let mut list = list.lock_mut();
        while list.len() >= MOST {
            list.remove(0);
        }
        list.push_cloned(toast.clone());
    }
    if kind != ToastKind::Error {
        // Count down in small steps, standing still while paused.
        wasm_bindgen_futures::spawn_local(async move {
            let mut left = STAYS_MS;
            while left > 0 {
                gloo_timers::future::TimeoutFuture::new(100).await;
                if !toast.paused.get() {
                    left = left.saturating_sub(100);
                }
            }
            dismiss(toast.id);
        });
    }
}

pub fn dismiss(id: u64) {
    toasts().lock_mut().retain(|toast| toast.id != id);
}

fn render(toast: &Rc<Toast>) -> View {
    let id = toast.id;
    let (pause_hover, resume_hover, pause_focus, resume_focus) = (
        toast.paused.clone(),
        toast.paused.clone(),
        toast.paused.clone(),
        toast.paused.clone(),
    );
    let card = el("div")
        .class("sh-toast")
        .class(toast.kind.class())
        .attr_if(toast.kind == ToastKind::Error, "role", "alert")
        .attr_from("data-paused", &toast.paused, |paused| {
            paused.then(String::new)
        })
        .child(icon(toast.kind.icon(), 20))
        .child(
            el("div")
                .class("sh-toast__body")
                .child(el("p").class("sh-toast__title").text(toast.title.clone()))
                .child(el("p").class("sh-toast__text").text(toast.text.clone())),
        )
        .child(
            icon_button(Icon::Close, "Dismiss")
                .class("sh-iconbtn-s")
                .hook(move |dom| dom.event(move |_: dominator::events::Click| dismiss(id))),
        )
        .hook(move |dom| {
            dom.event(move |_: dominator::events::MouseEnter| pause_hover.set_neq(true))
                .event(move |_: dominator::events::MouseLeave| resume_hover.set_neq(false))
                .event(move |_: dominator::events::FocusIn| pause_focus.set_neq(true))
                .event(move |_: dominator::events::FocusOut| resume_focus.set_neq(false))
                .event(move |event: dominator::events::KeyDown| {
                    if event.key() == "Escape" {
                        dismiss(id);
                    }
                })
        });
    let card = if toast.kind == ToastKind::Error {
        card
    } else {
        card.child(
            el("span")
                .class("sh-toast__timer")
                .attr("aria-hidden", "true")
                .child(el("span").class("sh-toast__timer-fill")),
        )
    };
    card.into()
}

/// Where toasts appear: once per page, at the root.
pub fn region() -> El {
    el("div")
        .class("sh-toasts")
        // A live region, not a landmark: a name on a plain container isn't
        // allowed, and an empty landmark on every page helps nobody.
        .attr("aria-live", "polite")
        .children_from(&toasts(), render)
}
