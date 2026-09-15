//! Every route in the app, and the copy each one shows.

/// A page in the app.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    Home,
    About,
    NotFound,
}

/// What a route shows, and how it presents itself to search engines and
/// link previews.
#[derive(Clone, Copy, Debug)]
pub struct Page {
    /// The document `<title>`, and the title in link previews.
    pub title: &'static str,
    /// The meta description, and the summary in link previews.
    pub description: &'static str,
    pub heading: &'static str,
    pub body: &'static [&'static str],
    /// A short highlighted note under the body, if any.
    pub note: Option<&'static str>,
}

impl Route {
    /// Every route a search engine should index, in sitemap and nav order.
    pub const INDEXABLE: &[Route] = &[Route::Home, Route::About];

    /// The route for a URL path. Unknown paths are `NotFound`.
    pub fn from_path(path: &str) -> Self {
        match path.trim_end_matches('/') {
            "" => Self::Home,
            "/about" => Self::About,
            _ => Self::NotFound,
        }
    }

    /// The canonical URL path, or `None` for a route with no URL of its own.
    pub fn path(self) -> Option<&'static str> {
        match self {
            Self::Home => Some("/"),
            Self::About => Some("/about"),
            Self::NotFound => None,
        }
    }

    /// The route's name in navigation.
    pub fn label(self) -> &'static str {
        match self {
            Self::Home => "Home",
            Self::About => "About",
            Self::NotFound => "Not found",
        }
    }

    pub fn page(self) -> Page {
        match self {
            Self::Home => Page {
                title: "Shonin · Verifiable trading strategies with pooled capital",
                description: "Shonin is an open platform for verifiable trading strategies with pooled capital. Coming soon.",
                heading: "Shonin",
                body: &["An open platform for verifiable trading strategies with pooled capital."],
                note: Some("Coming soon"),
            },
            Self::About => Page {
                title: "About · Shonin",
                description: "About Shonin.",
                heading: "About",
                body: &["About content here."],
                note: None,
            },
            Self::NotFound => Page {
                title: "Page not found · Shonin",
                description: "There's no page at this address.",
                heading: "Page not found",
                body: &["There's no page at this address."],
                note: None,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every indexable route's path must lead back to it, or the sitemap would
    /// list URLs that render as "not found".
    #[test]
    fn indexable_routes_round_trip_their_path() {
        for &route in Route::INDEXABLE {
            let path = route.path().expect("indexable routes have a path");
            assert_eq!(Route::from_path(path), route);
        }
    }

    #[test]
    fn not_found_is_never_indexed() {
        assert!(!Route::INDEXABLE.contains(&Route::NotFound));
        assert_eq!(Route::NotFound.path(), None);
    }

    #[test]
    fn paths_resolve() {
        assert_eq!(Route::from_path("/"), Route::Home);
        assert_eq!(Route::from_path("/about/"), Route::About);
        assert_eq!(Route::from_path("/nope"), Route::NotFound);
    }
}
