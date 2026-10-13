//! Pages: one module per route, composed only from components and atoms.

pub mod agent;
pub mod agents;
#[cfg(feature = "dev")]
pub mod atoms;
pub mod not_found;
pub mod portfolio;
pub mod publish;
pub mod soon;
#[cfg(any(feature = "dev", test))]
pub mod states;
pub mod status;
pub mod you;

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
        Route::Portfolio => portfolio::page(),
        Route::Claim { slug, id } => portfolio::claim(slug, *id),
        Route::Publish => publish::page(),
        Route::Job { id } => publish::job::page(id),
        Route::Activate { slug } => publish::activate::page(slug),
        Route::You => you::list::page(),
        Route::Manage { slug } => you::page(slug),
        Route::Status => status::page(),
        #[cfg(feature = "dev")]
        Route::Atoms => atoms::page(),
        #[cfg(feature = "dev")]
        Route::States => states::page(),
        #[cfg(not(feature = "dev"))]
        Route::Atoms | Route::States => not_found::page(),
        Route::NotFound => not_found::page(),
    }
}
