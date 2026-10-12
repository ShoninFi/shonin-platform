//! Atoms: the smallest pieces every page is built from. Each is a plain
//! function that takes values (or the store's `Mutable`s, for figures that
//! update in place) and returns a [`View`](crate::view::View), so the app and
//! the pre-render draw the same markup. Their styles live in the `ui` crate,
//! one stylesheet each (`packages/frontend/ui/styles/`).

pub mod badge;
pub mod button;
pub mod check;
pub mod delta;
pub mod dialog;
pub mod gauge;
pub mod hash;
pub mod icon;
pub mod logo;
pub mod notice;
pub mod prose;
pub mod rating;
pub mod segmented;
pub mod skeleton;
pub mod sparkline;
pub mod stat;
pub mod switch;
pub mod toast;
pub mod tooltip;

pub use badge::{Tone, badge};
pub use button::{Button, Kind, Size, icon_button};
pub use delta::{delta, delta_from};
pub use icon::icon;
pub use notice::{NoticeTone, notice};
pub use rating::{rating, review_chip};
