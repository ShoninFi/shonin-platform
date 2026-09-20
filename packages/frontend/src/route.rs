//! Every route in the app: its path, its title and description, and whether
//! search engines should index it.
//!
//! Routes are resolved against the data, so a path naming a strategy that
//! doesn't exist is `NotFound` rather than an empty page — and the build-time
//! pre-render writes one file per route, so a deep link is a real page with a
//! real status code rather than a soft 404.

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
    Accounts,
    Account { slug: String },
    Portfolio,
    Components,
    Component { slug: String },
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

/// An entry in the header's navigation.
pub struct NavItem {
    pub route: Route,
    pub label: &'static str,
}

impl Route {
    /// The primary navigation, in order.
    pub fn nav() -> Vec<NavItem> {
        vec![
            NavItem {
                route: Route::Strategies,
                label: "Strategies",
            },
            NavItem {
                route: Route::Accounts,
                label: "Accounts",
            },
            NavItem {
                route: Route::Components,
                label: "Components",
            },
            NavItem {
                route: Route::Queue,
                label: "Queue",
            },
            NavItem {
                route: Route::Verify,
                label: "Verify",
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
            ["strategies"] => Self::Strategies,
            ["strategies", slug] => Self::strategy(slug, Tab::Overview),
            ["strategies", slug, tab] => match Tab::from_segment(tab) {
                Some(tab) => Self::strategy(slug, tab),
                None => Self::NotFound,
            },
            ["authors", handle] => match data::author(handle) {
                Some(_) => Self::Author {
                    handle: (*handle).to_string(),
                },
                None => Self::NotFound,
            },
            ["accounts"] => Self::Accounts,
            ["accounts", slug] => match data::strategy(slug) {
                Some(strategy) if strategy.is_connected() => Self::Account {
                    slug: (*slug).to_string(),
                },
                _ => Self::NotFound,
            },
            ["portfolio"] => Self::Portfolio,
            ["components"] => Self::Components,
            ["components", slug] => match data::component(slug) {
                Some(_) => Self::Component {
                    slug: (*slug).to_string(),
                },
                None => Self::NotFound,
            },
            ["queue"] => Self::Queue,
            ["queue", id] => match data::job(id) {
                Some(_) => Self::Job {
                    id: (*id).to_string(),
                },
                None => Self::NotFound,
            },
            ["publish"] => Self::Publish,
            ["status"] => Self::Status,
            ["verify"] => Self::Verify,
            ["about"] => Self::About,
            _ => Self::NotFound,
        }
    }

    /// A vault strategy's page. Connected-account strategies live under
    /// `/accounts`, so they don't answer here.
    fn strategy(slug: &str, tab: Tab) -> Self {
        match data::strategy(slug) {
            Some(strategy) if !strategy.is_connected() => Self::Strategy {
                slug: slug.to_string(),
                tab,
            },
            _ => Self::NotFound,
        }
    }

    /// The canonical path, or `None` for a route with no URL of its own.
    pub fn path(&self) -> Option<String> {
        Some(match self {
            Self::Home => "/".to_string(),
            Self::Strategies => "/strategies".to_string(),
            Self::Strategy { slug, tab } => match tab.segment() {
                Some(segment) => format!("/strategies/{slug}/{segment}"),
                None => format!("/strategies/{slug}"),
            },
            Self::Author { handle } => format!("/authors/{handle}"),
            Self::Accounts => "/accounts".to_string(),
            Self::Account { slug } => format!("/accounts/{slug}"),
            Self::Portfolio => "/portfolio".to_string(),
            Self::Components => "/components".to_string(),
            Self::Component { slug } => format!("/components/{slug}"),
            Self::Queue => "/queue".to_string(),
            Self::Job { id } => format!("/queue/{id}"),
            Self::Publish => "/publish".to_string(),
            Self::Status => "/status".to_string(),
            Self::Verify => "/verify".to_string(),
            Self::About => "/about".to_string(),
            Self::NotFound => return None,
        })
    }

    /// Where this route sits in the navigation, if anywhere. A strategy page
    /// belongs under Strategies, a build under Queue, and so on.
    pub fn section(&self) -> Option<Route> {
        match self {
            Self::Strategies | Self::Strategy { .. } | Self::Author { .. } => {
                Some(Self::Strategies)
            }
            Self::Accounts | Self::Account { .. } => Some(Self::Accounts),
            Self::Components | Self::Component { .. } => Some(Self::Components),
            Self::Queue | Self::Job { .. } => Some(Self::Queue),
            Self::Verify => Some(Self::Verify),
            _ => None,
        }
    }

    /// Whether a search engine should index it. The portfolio is one viewer's
    /// own holdings, built in the browser, so it never is.
    pub fn is_indexable(&self) -> bool {
        !matches!(self, Self::NotFound | Self::Portfolio)
    }

    /// Every route the build pre-renders and the sitemap lists, in order.
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

        routes.push(Route::Accounts);
        for agent in data::agents() {
            routes.push(Route::Account {
                slug: agent.slug.clone(),
            });
        }

        routes.push(Route::Components);
        for component in &data::get().components {
            routes.push(Route::Component {
                slug: component.slug.clone(),
            });
        }

        routes.push(Route::Queue);
        for job in &data::get().jobs {
            routes.push(Route::Job { id: job.id.clone() });
        }

        for author in &data::get().authors {
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
                "Shonin · Verifiable trading strategies with pooled capital",
                "Pooled capital behind trading strategies whose code, custody and records are \
                 provable. Authors publish strategies as source; attested executors run them and \
                 can only move funds between a vault's own accounts.",
            ),
            Self::Strategies => page(
                "Strategies · Shonin",
                "Every strategy on the platform, ranked on live on-chain records normalised to \
                 one level of risk, with the size each record was earned at.",
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
                            "The bundle {name} runs, the source it was compiled from, its build \
                             log and the components it pins."
                        ),
                    ),
                    Tab::Review => page(
                        &format!("{name} review · Shonin"),
                        &format!(
                            "The attested review of {name}: a score for whether the code is \
                             reasonable, with the findings behind it."
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
                    "Every strategy {handle} has published — live, new, superseded, retired and \
                     the builds that failed — with their records."
                ),
            ),
            Self::Accounts => page(
                "Connected accounts · Shonin",
                "Published strategies that trade an account you keep at your own broker. The \
                 broker stops the strategy withdrawing anything; Shonin supplies the code's \
                 identity and its record.",
            ),
            Self::Account { slug } => {
                let Some(agent) = data::strategy(slug) else {
                    return Self::NotFound.page();
                };
                page(
                    &format!("{} · Connected accounts · Shonin", agent.name()),
                    &summarise(&agent.description),
                )
            }
            Self::Portfolio => page(
                "Your positions · Shonin",
                "What you hold across vaults and connected accounts.",
            ),
            Self::Components => page(
                "Components · Shonin",
                "Reusable pieces of a strategy: published with their source, pinned by hash, and \
                 shown with the capital running behind them.",
            ),
            Self::Component { slug } => {
                let Some(component) = data::component(slug) else {
                    return Self::NotFound.page();
                };
                page(
                    &format!("{} · Components · Shonin", component.name),
                    &component.summary,
                )
            }
            Self::Queue => page(
                "Build queue · Shonin",
                "Every build, first come first served, with its full log whether it succeeded or \
                 failed.",
            ),
            Self::Job { id } => {
                let Some(job) = data::job(id) else {
                    return Self::NotFound.page();
                };
                page(
                    &format!("Build {} · {} · Shonin", job.id, job.subject),
                    &format!(
                        "The build of {} by {}: its state, its recipe and its full log.",
                        job.subject, job.author
                    ),
                )
            }
            Self::Publish => page(
                "Publish a strategy · Shonin",
                "Submit a Rust project; an attested builder compiles it, an attested reviewer \
                 reads it, and its vault opens for deposits.",
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

    /// Every route the sitemap lists must lead back to itself, or it would be
    /// advertising URLs that render as "not found".
    #[test]
    fn indexable_routes_round_trip_their_path() {
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
        assert_eq!(Route::from_path("/"), Route::Home);
        assert_eq!(Route::from_path(""), Route::Home);
        assert_eq!(Route::from_path("/strategies"), Route::Strategies);
        assert_eq!(Route::from_path("/strategies/"), Route::Strategies);
        assert_eq!(
            Route::from_path("/strategies/funding-basis-v3"),
            Route::Strategy {
                slug: "funding-basis-v3".to_string(),
                tab: Tab::Overview
            }
        );
        assert_eq!(
            Route::from_path("/strategies/funding-basis-v3/review"),
            Route::Strategy {
                slug: "funding-basis-v3".to_string(),
                tab: Tab::Review
            }
        );
        assert_eq!(Route::from_path("/nope"), Route::NotFound);
    }

    /// A URL naming something that isn't there is a 404, not an empty page.
    #[test]
    fn unknown_names_are_not_found() {
        assert_eq!(
            Route::from_path("/strategies/no-such-thing"),
            Route::NotFound
        );
        assert_eq!(
            Route::from_path("/strategies/funding-basis-v3/nonsense"),
            Route::NotFound
        );
        assert_eq!(Route::from_path("/authors/nobody"), Route::NotFound);
        assert_eq!(Route::from_path("/components/nothing"), Route::NotFound);
        assert_eq!(Route::from_path("/queue/0"), Route::NotFound);
    }

    /// The two products have separate interfaces: a connected-account
    /// strategy is not a vault and doesn't answer at a vault's URL.
    #[test]
    fn the_two_products_keep_their_own_urls() {
        let agent = data::agents()[0].slug.clone();
        assert_eq!(
            Route::from_path(&format!("/accounts/{agent}")),
            Route::Account {
                slug: agent.clone()
            }
        );
        assert_eq!(
            Route::from_path(&format!("/strategies/{agent}")),
            Route::NotFound
        );

        let vault = data::vaults()[0].slug.clone();
        assert_eq!(
            Route::from_path(&format!("/accounts/{vault}")),
            Route::NotFound
        );
    }

    #[test]
    fn the_portfolio_is_never_indexed() {
        assert!(!Route::Portfolio.is_indexable());
        assert!(!Route::NotFound.is_indexable());
        assert!(!Route::indexable().contains(&Route::Portfolio));
    }

    #[test]
    fn navigation_points_at_real_pages() {
        for item in Route::nav() {
            let path = item.route.path().expect("a nav item has a path");
            assert_eq!(Route::from_path(&path), item.route);
        }
    }

    /// Every strategy tab is reachable, and the overview is the page itself.
    #[test]
    fn strategy_tabs_are_their_own_urls() {
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
