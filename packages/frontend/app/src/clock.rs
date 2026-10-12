//! The reader's clock, ticking once a second, for anything that counts time
//! (how far behind the figures are). Zero until the app starts it, which is
//! what the pre-render sees.

use futures_signals::signal::{Mutable, Signal};

thread_local! {
    static NOW: Mutable<f64> = Mutable::new(0.0);
}

/// Milliseconds since the Unix epoch, updated every second.
pub fn now() -> Mutable<f64> {
    NOW.with(Clone::clone)
}

pub fn signal() -> impl Signal<Item = f64> {
    now().signal()
}

/// Start ticking. The app calls this once.
pub fn start() {
    let now = now();
    now.set(crate::web::now_ms());
    gloo_timers::callback::Interval::new(1_000, move || now.set(crate::web::now_ms())).forget();
}
