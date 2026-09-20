//! The Shonin web app's routes and page copy.
//!
//! This library is plain Rust with no web dependencies, so two binaries share
//! it: the app itself (`src/main.rs`, wasm) and the build-time SEO tool
//! (`src/bin/seo.rs`, native). Adding a route here gives it a client-side page,
//! a pre-rendered HTML file and a sitemap entry.

pub mod data;
pub mod route;
