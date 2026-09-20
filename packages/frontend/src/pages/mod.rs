//! One module per page. `render` picks the one a route belongs to.

pub mod about;
pub mod account;
pub mod accounts;
pub mod author;
pub mod components;
pub mod home;
pub mod invest;
pub mod not_found;
pub mod parts;
pub mod portfolio;
pub mod publish;
pub mod queue;
pub mod status;
pub mod strategies;
pub mod strategy;
pub mod verify;

use dominator::Dom;

use crate::route::Route;

/// The page for a route.
pub fn render(route: &Route) -> Dom {
    match route {
        Route::Home => home::render(),
        Route::Strategies => strategies::render(),
        Route::Strategy { slug, tab } => strategy::render(slug, *tab),
        Route::Accounts => accounts::render(),
        Route::Account { slug } => account::render(slug),
        Route::Portfolio => portfolio::render(),
        Route::Author { handle } => author::render(handle),
        Route::Components => components::list(),
        Route::Component { slug } => components::detail(slug),
        Route::Queue => queue::list(),
        Route::Job { id } => queue::detail(id),
        Route::Publish => publish::render(),
        Route::Status => status::render(),
        Route::Verify => verify::render(),
        Route::About => about::render(),
        Route::NotFound => not_found::render(),
    }
}
