//! The pages, each with the file it is written to.

pub mod authors;
pub mod home;
pub mod legal;
pub mod not_found;

use crate::html::Page;

/// Every page and its file. A route `/x` is served from `x.html` (Workers
/// static assets drop the extension), `/` from `index.html`, and any path
/// with no file from `404.html` with a 404 status.
pub fn all() -> Vec<(&'static str, Page<'static>)> {
    vec![
        ("index.html", home::page()),
        ("authors.html", authors::page()),
        ("terms.html", legal::terms()),
        ("privacy.html", legal::privacy()),
        ("404.html", not_found::page()),
    ]
}
