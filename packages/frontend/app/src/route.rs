//! Every route in the app: its path, its title and description, and whether
//! search engines should index it.
//!
//! A route is read from the path (and, for the leaderboard, its query) alone.
//! Whether the thing it names exists is the page's business: the app reads a
//! live chain, and an agent missing from the snapshot it happens to hold may
//! simply not have arrived yet.

use crate::store;

/// The marketing site, which the lockup always links back to.
pub const SITE: &str = "https://shonin.fi/";
/// The documentation, a separate site.
pub const DOCS: &str = "https://docs.shonin.fi/";
/// How to read an agent page.
pub const DOCS_DEPOSITORS: &str = "https://docs.shonin.fi/depositors/";
/// Where "How it works" goes, from the first-visit strip.
pub const SITE_HOW: &str = "https://shonin.fi/#how";
pub const TERMS: &str = "https://shonin.fi/terms";
pub const PRIVACY: &str = "https://shonin.fi/privacy";
pub const GITHUB: &str = "https://github.com/ShoninFi";

/// One of an agent's tabs. Each is its own URL.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentTab {
    Overview,
    Review,
    Verify,
    Activity,
}

impl AgentTab {
    pub const ALL: [AgentTab; 4] = [
        AgentTab::Overview,
        AgentTab::Review,
        AgentTab::Verify,
        AgentTab::Activity,
    ];

    pub fn segment(self) -> Option<&'static str> {
        match self {
            Self::Overview => None,
            Self::Review => Some("review"),
            Self::Verify => Some("verify"),
            Self::Activity => Some("activity"),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Overview => "Overview",
            Self::Review => "Review",
            Self::Verify => "Verify",
            Self::Activity => "Activity",
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
    /// `/`: goes to the leaderboard.
    Home,
    /// `/agents`, optionally narrowed to one author (`?author=<chain>:<address>`).
    Agents {
        author: Option<String>,
    },
    Agent {
        slug: String,
        tab: AgentTab,
    },
    Portfolio,
    Publish,
    Status,
    /// Every token, type style, icon and atom; dev builds only.
    Atoms,
    NotFound,
}

/// What a route tells a search engine and a link preview.
#[derive(Clone, Debug)]
pub struct Page {
    pub title: String,
    pub description: String,
}

/// The longest a route's file name may be, in bytes, in any part of its path.
pub const FILE_NAME_BYTES: usize = 200;

/// The sections in the header and the phone tab bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    Agents,
    Portfolio,
    Publish,
}

impl Section {
    pub const ALL: [Section; 3] = [Section::Agents, Section::Portfolio, Section::Publish];

    pub fn label(self) -> &'static str {
        match self {
            Section::Agents => "Agents",
            Section::Portfolio => "Portfolio",
            Section::Publish => "Publish",
        }
    }

    pub fn href(self) -> &'static str {
        match self {
            Section::Agents => "/agents",
            Section::Portfolio => "/portfolio",
            Section::Publish => "/publish",
        }
    }
}

impl Route {
    /// The route for a URL's path and query (`?a=b`, with or without the `?`).
    pub fn from_url(path: &str, query: &str) -> Self {
        let segments: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
        match segments.as_slice() {
            [] => Self::Home,
            ["agents"] => Self::Agents {
                author: query_param(query, "author").filter(|key| !key.is_empty()),
            },
            ["agents", slug] => Self::agent(slug, AgentTab::Overview),
            ["agents", slug, tab] => match AgentTab::from_segment(tab) {
                Some(tab) => Self::agent(slug, tab),
                None => Self::NotFound,
            },
            ["portfolio"] => Self::Portfolio,
            ["publish"] => Self::Publish,
            ["status"] => Self::Status,
            ["_atoms"] if cfg!(feature = "dev") => Self::Atoms,
            _ => Self::NotFound,
        }
    }

    fn agent(slug: &str, tab: AgentTab) -> Self {
        match decode(slug) {
            Some(slug) if is_slug(&slug) => Self::Agent { slug, tab },
            _ => Self::NotFound,
        }
    }

    /// The canonical path, or `None` for a page with no URL of its own.
    pub fn path(&self) -> Option<String> {
        Some(match self {
            Self::Home => "/".to_string(),
            Self::Agents { author: None } => "/agents".to_string(),
            Self::Agents {
                author: Some(author),
            } => format!("/agents?author={}", encode(author)),
            Self::Agent { slug, tab } => match tab.segment() {
                Some(segment) => format!("/agents/{slug}/{segment}"),
                None => format!("/agents/{slug}"),
            },
            Self::Portfolio => "/portfolio".to_string(),
            Self::Publish => "/publish".to_string(),
            Self::Status => "/status".to_string(),
            Self::Atoms => "/_atoms".to_string(),
            Self::NotFound => return None,
        })
    }

    /// The file the pre-render writes this page to: the path with `.html`
    /// added (`agents.html`, `agents/base-1/review.html`), `404.html` for
    /// the page that isn't there, and none for a page that has no file of
    /// its own (the root redirects; a filtered leaderboard is the
    /// leaderboard's file).
    pub fn file(&self) -> Option<String> {
        match self {
            Self::NotFound => return Some("404.html".to_string()),
            Self::Home | Self::Atoms | Self::Agents { author: Some(_) } => return None,
            _ => {}
        }
        let path = self.path()?;
        let segments: Vec<&str> = path.trim_start_matches('/').split('/').collect();
        segments
            .iter()
            .all(|segment| {
                !segment.is_empty()
                    && *segment != "."
                    && *segment != ".."
                    && segment.len() <= FILE_NAME_BYTES
            })
            .then(|| format!("{}.html", segments.join("/")))
    }

    /// The section it sits under, for `aria-current` in the header.
    pub fn section(&self) -> Option<Section> {
        match self {
            Self::Agents { .. } | Self::Agent { .. } => Some(Section::Agents),
            Self::Portfolio => Some(Section::Portfolio),
            Self::Publish => Some(Section::Publish),
            _ => None,
        }
    }

    /// Whether the phone tab bar shows: the three root routes only.
    pub fn has_tab_bar(&self) -> bool {
        matches!(self, Self::Agents { .. } | Self::Portfolio | Self::Publish)
    }

    /// The title the phone header shows beside the mark. The leaderboard
    /// shows the lockup instead.
    pub fn phone_title(&self) -> Option<String> {
        match self {
            Self::Agents { .. } | Self::Home => None,
            Self::Agent { slug, .. } => Some(
                store::get()
                    .agent(slug)
                    .map(|agent| agent.title.get_cloned())
                    .unwrap_or_else(|| "Agent".to_string()),
            ),
            Self::Portfolio => Some("Portfolio".to_string()),
            Self::Publish => Some("Publish".to_string()),
            Self::Status => Some("Status".to_string()),
            Self::Atoms => Some("Atoms".to_string()),
            Self::NotFound => Some("Not found".to_string()),
        }
    }

    /// Whether search engines should list it. A filtered list, someone's
    /// portfolio and the pages still being built are not.
    pub fn is_indexable(&self) -> bool {
        matches!(
            self,
            Self::Agents { author: None } | Self::Agent { .. } | Self::Status
        )
    }

    /// Every page the pre-render writes a file for, from the store as it is.
    pub fn all_pages() -> Vec<Route> {
        let mut routes = vec![Route::Agents { author: None }];
        for agent in store::get().all() {
            for tab in AgentTab::ALL {
                routes.push(Route::Agent {
                    slug: agent.slug.clone(),
                    tab,
                });
            }
        }
        routes.extend([Route::Portfolio, Route::Publish, Route::Status]);
        routes
    }

    /// The page's title and description.
    pub fn page(&self) -> Page {
        let page = |title: String, description: &str| Page {
            title,
            description: description.to_string(),
        };
        match self {
            Self::Home | Self::Agents { author: None } => page(
                "Agents · Shonin".to_string(),
                "Every agent's review, record and exit, read from its own contracts.",
            ),
            Self::Agents { author: Some(_) } => page(
                "Agents by one Author · Shonin".to_string(),
                "One Author's agents: each with its review, record and exit, read from its own contracts.",
            ),
            Self::Agent { slug, tab } => {
                let name = store::get()
                    .agent(slug)
                    .map(|agent| agent.title.get_cloned())
                    .unwrap_or_else(|| slug.clone());
                let title = match tab {
                    AgentTab::Overview => format!("{name} · Shonin"),
                    tab => format!("{name}: {} · Shonin", tab.label()),
                };
                page(
                    title,
                    "An agent's review, record and exit, read from its own contracts.",
                )
            }
            Self::Portfolio => page(
                "Portfolio · Shonin".to_string(),
                "Your shares, what they're worth and any booked withdrawals.",
            ),
            Self::Publish => page(
                "Publish an agent · Shonin".to_string(),
                "Publish a trading agent for $1: reviewed, built in an attested machine, then yours to activate.",
            ),
            Self::Status => page(
                "Status · Shonin".to_string(),
                "Every kill, freeze and allowlist change, as it happens.",
            ),
            Self::Atoms => page(
                "Atoms · Shonin".to_string(),
                "Every token, type style, icon and atom.",
            ),
            Self::NotFound => page(
                "Page not found · Shonin".to_string(),
                "This page isn't on the chain, or anywhere else.",
            ),
        }
    }
}

/// Whether a name is a slug as the API writes them: `base-12`.
fn is_slug(slug: &str) -> bool {
    !slug.is_empty()
        && slug.len() <= FILE_NAME_BYTES
        && slug
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// One parameter from a query string, percent-decoded.
pub fn query_param(query: &str, name: &str) -> Option<String> {
    query
        .trim_start_matches('?')
        .split('&')
        .filter_map(|pair| pair.split_once('=').or(Some((pair, ""))))
        .find(|(key, _)| *key == name)
        .and_then(|(_, value)| decode(&value.replace('+', " ")))
}

/// Percent-decode a URL part; `None` if it isn't valid UTF-8 once decoded.
pub fn decode(part: &str) -> Option<String> {
    let bytes = part.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = std::str::from_utf8(bytes.get(index + 1..index + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// Percent-encode a query value: everything but unreserved characters and `:`.
pub fn encode(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.~:".contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_read_back_as_the_routes_that_wrote_them() {
        for route in [
            Route::Agents { author: None },
            Route::Agents {
                author: Some("base:0x07cc6a64a539792bb53fd29e3118bf6d17a1f31a".to_string()),
            },
            Route::Agent {
                slug: "base-1".to_string(),
                tab: AgentTab::Overview,
            },
            Route::Agent {
                slug: "solana-3".to_string(),
                tab: AgentTab::Activity,
            },
            Route::Portfolio,
            Route::Publish,
            Route::Status,
        ] {
            let path = route.path().unwrap();
            let (path, query) = path.split_once('?').unwrap_or((&path, ""));
            assert_eq!(Route::from_url(path, query), route, "{path}?{query}");
        }
    }

    #[test]
    fn an_unknown_path_is_not_found() {
        assert_eq!(Route::from_url("/strategies", ""), Route::NotFound);
        assert_eq!(Route::from_url("/agents/base-1/code", ""), Route::NotFound);
        assert_eq!(Route::from_url("/agents/..%2Fx", ""), Route::NotFound);
        assert_eq!(Route::from_url("/agents/a%20b", ""), Route::NotFound);
    }

    #[test]
    fn the_atoms_page_exists_only_in_dev_builds() {
        let atoms = Route::from_url("/_atoms", "");
        if cfg!(feature = "dev") {
            assert_eq!(atoms, Route::Atoms);
        } else {
            assert_eq!(atoms, Route::NotFound);
        }
        assert_eq!(Route::Atoms.file(), None);
    }

    #[test]
    fn the_root_has_no_page_of_its_own() {
        assert_eq!(Route::from_url("/", ""), Route::Home);
        assert_eq!(Route::Home.file(), None);
    }

    #[test]
    fn files_are_flat_html_beside_their_parents() {
        assert_eq!(
            Route::Agents { author: None }.file().as_deref(),
            Some("agents.html")
        );
        assert_eq!(
            Route::Agent {
                slug: "base-1".into(),
                tab: AgentTab::Review
            }
            .file()
            .as_deref(),
            Some("agents/base-1/review.html")
        );
        assert_eq!(Route::NotFound.file().as_deref(), Some("404.html"));
    }

    #[test]
    fn a_name_too_long_for_a_file_name_has_no_file() {
        let long = "a".repeat(FILE_NAME_BYTES + 1);
        assert_eq!(
            Route::from_url(&format!("/agents/{long}"), ""),
            Route::NotFound
        );
        let longest = Route::Agent {
            slug: "a".repeat(FILE_NAME_BYTES),
            tab: AgentTab::Overview,
        };
        assert!(longest.file().is_some());
    }

    #[test]
    fn an_author_filter_survives_the_query_string() {
        assert_eq!(
            Route::from_url("/agents", "?author=base%3A0xABC&x=1"),
            Route::Agents {
                author: Some("base:0xABC".to_string())
            }
        );
        assert_eq!(query_param("a=1&b", "b").as_deref(), Some(""));
        assert_eq!(decode("%E2%88%92").as_deref(), Some("\u{2212}"));
        assert_eq!(decode("%zz"), None);
    }

    #[test]
    fn only_public_pages_are_indexed() {
        assert!(Route::Agents { author: None }.is_indexable());
        assert!(Route::Status.is_indexable());
        assert!(!Route::Portfolio.is_indexable());
        assert!(!Route::NotFound.is_indexable());
        assert!(
            !Route::Agents {
                author: Some("base:0x1".into())
            }
            .is_indexable()
        );
    }

    #[test]
    fn the_tab_bar_shows_on_the_three_root_routes_only() {
        assert!(Route::Agents { author: None }.has_tab_bar());
        assert!(Route::Portfolio.has_tab_bar());
        assert!(Route::Publish.has_tab_bar());
        assert!(!Route::Status.has_tab_bar());
        assert!(!Route::NotFound.has_tab_bar());
        assert!(
            !Route::Agent {
                slug: "base-1".into(),
                tab: AgentTab::Overview
            }
            .has_tab_bar()
        );
    }
}
