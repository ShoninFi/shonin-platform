//! The Shonin web app.
//!
//! Everything is written once, as [`view::View`]s built from the store's
//! signals, so two binaries share it: the app (`src/main.rs`, wasm), which
//! draws live Dominator trees, and the build-time pre-render
//! (`src/bin/seo.rs`, native), which writes the same views as static HTML so
//! every page reads with JavaScript off.
//!
//! Layers: [`atoms`] (one file each) → [`components`] (the page chrome) →
//! [`pages`] (one module per route). The store ([`store`]) holds the latest
//! snapshot as keyed `Mutable`s; [`api`] keeps it fresh.

pub mod api;
pub mod atoms;
pub mod clock;
pub mod components;
pub mod format;
pub mod freshness;
pub mod model;
pub mod money;
pub mod pages;
pub mod route;
pub mod shell;
pub mod store;
pub mod tx;
pub mod view;
pub mod wallet;
pub mod web;
