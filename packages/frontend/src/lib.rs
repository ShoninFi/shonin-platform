//! The Shonin web app.
//!
//! Everything but the views themselves is plain Rust with no web
//! dependencies, so two binaries share it: the app (`src/main.rs`, wasm) and
//! the build-time pre-render (`src/bin/seo.rs`, native). Adding a route in
//! `route.rs` gives it a page in the app, a pre-rendered HTML file and a
//! sitemap entry.

pub mod copy;
pub mod data;
pub mod format;
pub mod pages;
pub mod prerender;
pub mod route;
pub mod state;
pub mod ui;
