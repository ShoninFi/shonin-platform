//! Pages: one module per route, composed only from components and atoms.

pub mod agent;
pub mod agents;
#[cfg(feature = "dev")]
pub mod atoms;
pub mod not_found;
pub mod soon;
pub mod status;

use crate::route::Route;
use crate::view::View;

/// What a page puts on the screen: anything that sits between the header and
/// the main column (the first-visit strip), and the main column itself.
pub struct Page {
    pub top: Option<View>,
    pub body: View,
}

/// The page for a route. `strip_dismissed` is whether this browser dismissed
/// the first-visit strip (always false in the pre-render).
pub fn page(route: &Route, strip_dismissed: bool) -> Page {
    match route {
        Route::Home => agents::page(None, strip_dismissed),
        Route::Agents { author } => agents::page(author.clone(), strip_dismissed),
        Route::Agent { slug, tab } => agent::page(slug, *tab),
        Route::Portfolio => soon::portfolio(),
        Route::Publish => soon::publish(),
        Route::Status => status::page(),
        #[cfg(feature = "dev")]
        Route::Atoms => atoms::page(),
        #[cfg(not(feature = "dev"))]
        Route::Atoms => not_found::page(),
        Route::NotFound => not_found::page(),
    }
}
