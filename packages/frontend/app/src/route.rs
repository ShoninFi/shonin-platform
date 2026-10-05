//! Every route in the app: its path, its title and description, and whether
//! search engines should index it.
//!
//! A route is read from the path alone. Whether the thing it names exists is
//! the page's business, not the router's: the app is looking at a live chain,
//! and a strategy that is not in the snapshot the app happens to hold may
//! simply not have arrived yet.

use crate::data;

/// One of a strategy's tabs. Each is its own URL, so a link can point at a
/// vault's record or its review directly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Overview,
    Record,
    Code,
    Review,
    Verify,
    Health,
}

impl Tab {
    /// Every tab, in the order they appear on a strategy page.
    pub const ALL: [Tab; 6] = [
        Tab::Overview,
        Tab::Record,
        Tab::Code,
        Tab::Review,
        Tab::Verify,
        Tab::Health,
    ];

    /// The path segment under the strategy. Overview has none: it is the page.
    pub fn segment(self) -> Option<&'static str> {
        match self {
            Self::Overview => None,
            Self::Record => Some("record"),
            Self::Code => Some("code"),
            Self::Review => Some("review"),
            Self::Verify => Some("verify"),
            Self::Health => Some("health"),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Overview => "Overview",
            Self::Record => "Record",
            Self::Code => "Code",
            Self::Review => "Review",
            Self::Verify => "Which code is trading this?",
            Self::Health => "Health",
        }
    }

    /// The short label, for a narrow screen.
    pub fn short_label(self) -> &'static str {
        match self {
            Self::Verify => "Verify",
            other => other.label(),
        }
    }

    fn from_segment(segment: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|tab| tab.segment() == Some(segment))
    }
}

/// A page in the app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Route {
    Home,
    Strategies,
    Strategy { slug: String, tab: Tab },
    Author { handle: String },
    Portfolio,
    Queue,
    Job { id: String },
    Publish,
    Status,
    Verify,
    About,
    NotFound,
}

/// What a route tells a search engine and a link preview.
#[derive(Clone, Debug)]
pub struct Page {
    pub title: String,
    pub description: String,
}

/// The handbook: the same system explained at length, on a site of its own.
pub const HANDBOOK: &str = "https://docs.shonin.fi/";

/// An entry in the header's navigation. Most name a page of this app; the
/// handbook is a separate site, so that one names a URL instead.
pub enum NavItem {
    Page {
        route: Route,
        label: &'static str,
    },
    Site {
        href: &'static str,
        label: &'static str,
    },
}

impl NavItem {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Page { label, .. } | Self::Site { label, .. } => label,
        }
    }

    /// Where the link goes.
    pub fn href(&self) -> String {
        match self {
            Self::Page { route, .. } => route.path().unwrap_or_else(|| "/".to_string()),
            Self::Site { href, .. } => (*href).to_string(),
        }
    }

    /// Whether this is the section the reader is in. A link off the site is
    /// never the current one, whatever page they are on.
    pub fn is_current(&self, section: Option<&Route>) -> bool {
        match self {
            Self::Page { route, .. } => Some(route) == section,
            Self::Site { .. } => false,
        }
    }
}

impl Route {
    /// The primary navigation, in order.
    pub fn nav() -> Vec<NavItem> {
        vec![
            NavItem::Page {
                route: Route::Strategies,
                label: "Agents",
            },
            NavItem::Page {
                route: Route::Queue,
                label: "Queue",
            },
            NavItem::Page {
                route: Route::Verify,
                label: "Verify",
            },
            NavItem::Site {
                href: HANDBOOK,
                label: "Docs",
            },
        ]
    }

    /// The route for a URL path. Unknown paths, and paths naming something
    /// that doesn't exist, are `NotFound`.
    pub fn from_path(path: &str) -> Self {
        let trimmed = path.trim_end_matches('/');
        let segments: Vec<&str> = trimmed.split('/').filter(|part| !part.is_empty()).collect();

        match segments.as_slice() {
            [] => Self::Home,
            ["agents"] => Self::Strategies,
            ["agents", slug] => Self::strategy(slug, Tab::Overview),
            ["agents", slug, tab] => match Tab::from_segment(tab) {
                Some(tab) => Self::strategy(slug, tab),
                None => Self::NotFound,
            },
            ["authors", handle] => match decode_segment(handle) {
                Some(handle) => Self::Author { handle },
                None => Self::NotFound,
            },
            ["portfolio"] => Self::Portfolio,
            ["queue"] => Self::Queue,
            ["queue", id] => Self::Job {
                id: (*id).to_string(),
            },
            ["publish"] => Self::Publish,
            ["status"] => Self::Status,
            ["verify"] => Self::Verify,
            ["about"] => Self::About,
            _ => Self::NotFound,
        }
    }

    fn strategy(slug: &str, tab: Tab) -> Self {
        Self::Strategy {
            slug: slug.to_string(),
            tab,
        }
    }

    /// The canonical path, or `None` for a route with no URL of its own.
    pub fn path(&self) -> Option<String> {
        Some(match self {
            Self::Home => "/".to_string(),
            Self::Strategies => "/agents".to_string(),
            Self::Strategy { slug, tab } => match tab.segment() {
                Some(segment) => format!("/agents/{slug}/{segment}"),
                None => format!("/agents/{slug}"),
            },
            Self::Author { handle } => author_path(handle),
            Self::Portfolio => "/portfolio".to_string(),
            Self::Queue => "/queue".to_string(),
            Self::Job { id } => format!("/queue/{id}"),
            Self::Publish => "/publish".to_string(),
            Self::Status => "/status".to_string(),
            Self::Verify => "/verify".to_string(),
            Self::About => "/about".to_string(),
            Self::NotFound => return None,
        })
    }

    /// The file the build writes this route's page to, relative to the output
    /// directory: `index.html` for the home page, `404.html` for a page that
    /// isn't there, and the path with `.html` added for the rest.
    ///
    /// A static host decodes the request path before it looks for the file,
    /// so the file is named after the decoded path: `/authors/bob%20b` is
    /// `authors/bob b.html`. A route whose decoded path would leave its own
    /// directory (a name that is empty, `.` or `..`, or that holds a slash)
    /// has no file, and `None` says so: no request could reach it anyway, and
    /// writing it would put a file somewhere it doesn't belong.
    pub fn file(&self) -> Option<String> {
        let Some(path) = self.path() else {
            return Some("404.html".to_string());
        };
        if path == "/" {
            return Some("index.html".to_string());
        }
        let segments = path
            .trim_start_matches('/')
            .split('/')
            .map(decode_segment)
            .collect::<Option<Vec<String>>>()?;
        let stays_put = |segment: &String| {
            !segment.is_empty()
                && segment != "."
                && segment != ".."
                && !segment.contains(['/', '\\', '\0'])
        };
        segments
            .iter()
            .all(stays_put)
            .then(|| format!("{}.html", segments.join("/")))
    }

    /// Where this route sits in the navigation, if anywhere. A strategy page
    /// belongs under Strategies, a build under Queue, and so on.
    pub fn section(&self) -> Option<Route> {
        match self {
            Self::Strategies | Self::Strategy { .. } | Self::Author { .. } => {
                Some(Self::Strategies)
            }
            Self::Queue | Self::Job { .. } => Some(Self::Queue),
            Self::Verify => Some(Self::Verify),
            _ => None,
        }
    }

    /// Whether a search engine should index it. The portfolio is one viewer's
    /// own holdings, read from the chain for one address, so it never is.
    pub fn is_indexable(&self) -> bool {
        !matches!(self, Self::NotFound | Self::Portfolio)
    }

    /// Every route the build writes a file for: the indexable ones, plus the
    /// pages that are real but shouldn't be listed. Without a file, opening
    /// one of those directly would answer 404 before the app could render it.
    pub fn all_pages() -> Vec<Route> {
        let mut routes = Self::indexable();
        routes.push(Route::Portfolio);
        routes
    }

    /// Every route the sitemap lists, in order.
    pub fn indexable() -> Vec<Route> {
        let mut routes = vec![Route::Home, Route::Strategies];

        for strategy in data::vaults() {
            for tab in Tab::ALL {
                routes.push(Route::Strategy {
                    slug: strategy.slug.clone(),
                    tab,
                });
            }
        }

        routes.push(Route::Queue);
        for job in data::jobs() {
            routes.push(Route::Job { id: job.id.clone() });
        }

        for author in data::authors() {
            routes.push(Route::Author {
                handle: author.handle.clone(),
            });
        }

        routes.extend([Route::Publish, Route::Status, Route::Verify, Route::About]);
        routes
    }

    /// The page's title and description.
    pub fn page(&self) -> Page {
        let page = |title: &str, description: &str| Page {
            title: title.to_string(),
            description: description.to_string(),
        };

        match self {
            Self::Home => page(
                "Shonin · Verifiable trading agents with pooled capital",
                "Pooled capital behind trading agents whose code, custody and records are \
                 provable. Authors publish agents as source; attested executors run them and \
                 can only move funds between a vault's own accounts.",
            ),
            Self::Strategies => page(
                "Agents · Shonin",
                "Every agent on the platform, with its return over 30 days and since opening, \
                 its deepest drop and the money in its vault, all read from the chain.",
            ),
            Self::Strategy { slug, tab } => {
                let Some(strategy) = data::strategy(slug) else {
                    return Self::NotFound.page();
                };
                let name = strategy.name();
                match tab {
                    Tab::Overview => page(
                        &format!("{name} · Shonin"),
                        &summarise(&strategy.description),
                    ),
                    Tab::Record => page(
                        &format!("{name} record · Shonin"),
                        &format!(
                            "The on-chain record of {name}: value per share read from the vault \
                             contract, its drawdown, the capital behind it, and every trade."
                        ),
                    ),
                    Tab::Code => page(
                        &format!("{name} code · Shonin"),
                        &format!(
                            "The bundle {name} runs, the source it was compiled from, the \
                             recipe it was built with and the hashes that tie them together."
                        ),
                    ),
                    Tab::Review => page(
                        &format!("{name} review · Shonin"),
                        &format!(
                            "The attested review of {name}: six areas rated, with the findings \
                             behind each."
                        ),
                    ),
                    Tab::Verify => page(
                        &format!("Which code is trading {name} · Shonin"),
                        &format!(
                            "The image, the blessed keys and the venue approvals behind {name}, \
                             and how to check each one yourself."
                        ),
                    ),
                    Tab::Health => page(
                        &format!("{name} health · Shonin"),
                        &format!(
                            "How {name} is running: restarts, rejections, venue errors, quota \
                             hits and tank runway, each against its threshold."
                        ),
                    ),
                }
            }
            Self::Author { handle } => page(
                &format!("{handle} · Shonin"),
                &format!(
                    "Every agent {handle} has published, live or stopped, with their records."
                ),
            ),
            Self::Portfolio => page(
                "Your positions · Shonin",
                "What you hold across vaults, read from the chain for the address you \
                 connected.",
            ),
            Self::Queue => page(
                "Publishing queue · Shonin",
                "Every publishing job, first come first served, and how each one ended.",
            ),
            Self::Job { id } => {
                let Some(job) = data::job(id) else {
                    return Self::NotFound.page();
                };
                page(
                    &format!("Job {} · {} · Shonin", job.id, job.title),
                    &format!(
                        "Publishing {} by {}: its review, its build, and how it ended.",
                        job.title, job.author
                    ),
                )
            }
            Self::Publish => page(
                "Publish an agent · Shonin",
                "Submit a Rust project for $1: an attested reviewer reads it, an attested \
                 builder compiles it, and you activate its vault when you're ready.",
            ),
            Self::Status => page(
                "Status · Shonin",
                "Every kill with its reason, every change to the image allowlist, and every \
                 venue that stopped behaving.",
            ),
            Self::Verify => page(
                "Verify · Shonin",
                "How anyone checks which code is trading a vault, that its bundle came from its \
                 source, that its trades are real, and what it is worth.",
            ),
            Self::About => page(
                "About · Shonin",
                "What Shonin is, what it guarantees, and what it deliberately does not promise.",
            ),
            Self::NotFound => page(
                "Page not found · Shonin",
                "There's no page at this address.",
            ),
        }
    }
}

/// The path of an author's page. The name is whatever the author is shown
/// as, spaces, brackets and all, so it goes into the URL encoded.
pub fn author_path(handle: &str) -> String {
    format!("/authors/{}", encode_segment(handle))
}

/// Text as one URL path segment: everything outside the characters RFC 3986
/// leaves unreserved (letters, digits, `-`, `.`, `_`, `~`) is percent-encoded,
/// so a `/`, `?`, `#` or `%` in it can't end the segment or start another.
pub fn encode_segment(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// A URL path segment as the text it encodes, or `None` if it is not a valid
/// encoding of UTF-8 text.
pub fn decode_segment(segment: &str) -> Option<String> {
    let bytes = segment.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = std::str::from_utf8(bytes.get(index + 1..index + 3)?).ok()?;
            if !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return None;
            }
            out.push(u8::from_str_radix(hex, 16).ok()?);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// The first sentence or two of a description, for a meta tag.
fn summarise(description: &str) -> String {
    const LIMIT: usize = 240;
    if description.len() <= LIMIT {
        return description.to_string();
    }
    let cut = description[..LIMIT]
        .rfind(". ")
        .map_or(LIMIT, |index| index + 1);
    description[..cut].trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn platform() {
        crate::fixture::install();
    }

    /// Every route the sitemap lists must lead back to itself, or it would be
    /// advertising URLs that render as "not found".
    #[test]
    fn indexable_routes_round_trip_their_path() {
        platform();
        for route in Route::indexable() {
            let path = route.path().expect("an indexable route has a path");
            assert_eq!(
                Route::from_path(&path),
                route,
                "{path} doesn't resolve back to itself"
            );
        }
    }

    #[test]
    fn every_indexable_route_says_something_about_itself() {
        platform();
        for route in Route::indexable() {
            let page = route.page();
            assert!(page.title.contains("Shonin"), "{page:?} has no site name");
            assert!(
                page.description.len() > 40,
                "{:?} has a thin description",
                route.path()
            );
        }
    }

    #[test]
    fn paths_resolve() {
        platform();
        assert_eq!(Route::from_path("/"), Route::Home);
        assert_eq!(Route::from_path(""), Route::Home);
        assert_eq!(Route::from_path("/agents"), Route::Strategies);
        assert_eq!(Route::from_path("/agents/"), Route::Strategies);
        assert_eq!(
            Route::from_path("/agents/funding-basis-v3"),
            Route::Strategy {
                slug: "funding-basis-v3".to_string(),
                tab: Tab::Overview
            }
        );
        assert_eq!(
            Route::from_path("/agents/funding-basis-v3/review"),
            Route::Strategy {
                slug: "funding-basis-v3".to_string(),
                tab: Tab::Review
            }
        );
        assert_eq!(Route::from_path("/nope"), Route::NotFound);
    }

    /// A path the app cannot read at all is a 404. A path naming a strategy
    /// is not: the app is looking at a live chain, and a strategy that is not
    /// in the snapshot it happens to hold may simply not have arrived yet, so
    /// the page says so rather than the router pretending the URL is wrong.
    #[test]
    fn unknown_names_are_a_page_rather_than_a_dead_route() {
        assert!(matches!(
            Route::from_path("/agents/no-such-thing"),
            Route::Strategy { .. }
        ));
        assert_eq!(
            Route::from_path("/agents/funding-basis-v3/nonsense"),
            Route::NotFound
        );
        assert!(matches!(
            Route::from_path("/authors/nobody"),
            Route::Author { .. }
        ));
        assert!(matches!(Route::from_path("/queue/0"), Route::Job { .. }));

        // A path the app has no page for at all is still a real 404.
        assert_eq!(Route::from_path("/accounts"), Route::NotFound);
        assert_eq!(Route::from_path("/components"), Route::NotFound);
    }

    /// The portfolio is one address's own holdings, so
    /// it is never listed — but it still needs a file of its own, or opening
    /// it directly would answer "not found".
    #[test]
    fn the_portfolio_has_a_page_but_is_never_indexed() {
        platform();
        assert!(!Route::Portfolio.is_indexable());
        assert!(!Route::NotFound.is_indexable());
        assert!(!Route::indexable().contains(&Route::Portfolio));
        assert!(Route::all_pages().contains(&Route::Portfolio));
    }

    /// Everything the sitemap lists is also written as a file.
    #[test]
    fn every_listed_route_is_written() {
        platform();
        let pages = Route::all_pages();
        for route in Route::indexable() {
            assert!(
                pages.contains(&route),
                "{:?} is listed but not written",
                route.path()
            );
        }
    }

    #[test]
    fn navigation_points_at_real_pages() {
        platform();
        for item in Route::nav() {
            match item {
                NavItem::Page { route, .. } => {
                    let path = route.path().expect("a nav item has a path");
                    assert_eq!(Route::from_path(&path), route);
                }
                // An outward link is not this app's to resolve; all the router
                // can say is that it leaves the site rather than looking like
                // a path it would try to route.
                NavItem::Site { href, .. } => {
                    assert!(href.starts_with("https://"), "{href} is not a URL");
                }
            }
        }
    }

    /// An author's name is whatever they are shown as, so it can hold anything
    /// a URL treats specially. It goes into the path as one segment and comes
    /// back out unchanged.
    #[test]
    fn an_awkward_author_name_round_trips_its_path() {
        let route = Route::Author {
            handle: crate::fixture::AWKWARD_NAME.to_string(),
        };
        let path = route.path().unwrap();
        assert_eq!(
            path,
            "/authors/zo%C3%AB%2Fal%20%280x12ab%E2%80%A6%29%20100%25%20%231%3F%20on%20Base"
        );
        assert_eq!(Route::from_path(&path), route);
        assert_eq!(Route::from_path(&format!("{path}/")), route);

        // A browser leaves some characters as they are rather than encoding
        // them; those read back the same.
        assert_eq!(
            Route::from_path("/authors/bob%20(0x12)"),
            Route::Author {
                handle: "bob (0x12)".to_string()
            }
        );
    }

    #[test]
    fn a_bad_encoding_is_not_found() {
        for path in [
            "/authors/bob%",
            "/authors/bob%2",
            "/authors/bob%zz",
            "/authors/%FF%FE",
            "/authors/%C3",
        ] {
            assert_eq!(Route::from_path(path), Route::NotFound, "{path}");
        }
    }

    #[test]
    fn segments_encode_only_what_they_must() {
        assert_eq!(encode_segment("Az09-._~"), "Az09-._~");
        assert_eq!(encode_segment("a b/c?d#e%f+g"), "a%20b%2Fc%3Fd%23e%25f%2Bg");
        assert_eq!(decode_segment("a%20b%2fc").as_deref(), Some("a b/c"));
        assert_eq!(decode_segment("%"), None);
    }

    /// A static host decodes a request before it looks for the file, so the
    /// file is written under the decoded name.
    #[test]
    fn a_page_is_written_where_its_url_is_looked_for() {
        assert_eq!(Route::Home.file().as_deref(), Some("index.html"));
        assert_eq!(Route::NotFound.file().as_deref(), Some("404.html"));
        assert_eq!(
            Route::Strategy {
                slug: "funding-basis-v3".to_string(),
                tab: Tab::Record
            }
            .file()
            .as_deref(),
            Some("agents/funding-basis-v3/record.html")
        );
        assert_eq!(
            Route::Author {
                handle: "bob (0x12ab…) on Base".to_string()
            }
            .file()
            .as_deref(),
            Some("authors/bob (0x12ab…) on Base.html")
        );
    }

    /// A name that would put its page outside its own directory gets no file.
    #[test]
    fn a_name_cannot_write_outside_its_directory() {
        for handle in ["..", ".", "", "a/b", "../../etc/passwd", "a\\b", "a\0b"] {
            let route = Route::Author {
                handle: handle.to_string(),
            };
            assert_eq!(route.file(), None, "{handle:?}");
        }
    }

    /// Authors named awkwardly are still listed, and their paths lead back to
    /// them. A name holding a slash has no file of its own: decoded, it
    /// would be a directory.
    #[test]
    fn awkward_names_are_listed_and_resolve() {
        let names = crate::fixture::install_awkward_names();
        for name in &names {
            let route = Route::Author {
                handle: name.clone(),
            };
            assert!(Route::indexable().contains(&route), "{name} isn't listed");
            assert_eq!(route.file(), None, "{name} would be written as a directory");

            let without_slash = Route::Author {
                handle: name.replace('/', " "),
            };
            assert_eq!(
                without_slash.file(),
                Some(format!("authors/{}.html", name.replace('/', " "))),
            );
        }
        for route in Route::indexable() {
            let path = route.path().unwrap();
            assert_eq!(Route::from_path(&path), route, "{path}");
        }
    }

    /// Every strategy tab is reachable, and the overview is the page itself.
    #[test]
    fn strategy_tabs_are_their_own_urls() {
        crate::fixture::install();
        let slug = data::vaults()[0].slug.clone();
        for tab in Tab::ALL {
            let route = Route::Strategy {
                slug: slug.clone(),
                tab,
            };
            let path = route.path().unwrap();
            assert_eq!(Route::from_path(&path), route);
        }
        assert_eq!(Tab::Overview.segment(), None);
    }
}
