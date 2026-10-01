//! The leaderboard: a plain, sortable table of facts each vault's contract
//! records, and the rules the list is built by.

use std::rc::Rc;
use std::sync::OnceLock;

use dominator::{Dom, html};
use futures_signals::map_ref;
use futures_signals::signal::Mutable;
use shared::{ExitClass, Source, Standing, Strategy};

use crate::copy;
use crate::data;
use crate::format as fmt;
use crate::pages::parts;
use crate::ui::form;
use crate::ui::primitives as ui;
use crate::ui::table::{Column, Row, Sort, sortable_table};
use crate::ui::tabs;

/// Which part of the list is on screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum View {
    Listed,
    New,
    Past,
}

struct Controls {
    view: Mutable<View>,
    sort: Sort,
    chain: Mutable<String>,
    source: Mutable<String>,
    exit: Mutable<String>,
}

/// The leaderboard remembers how it was left for as long as the app is open,
/// so going into a strategy and back doesn't lose a sort or a filter.
fn controls() -> &'static Controls {
    static CONTROLS: OnceLock<Controls> = OnceLock::new();
    CONTROLS.get_or_init(|| Controls {
        view: Mutable::new(View::Listed),
        sort: Sort::new("month"),
        chain: Mutable::new("all".to_string()),
        source: Mutable::new("all".to_string()),
        exit: Mutable::new("all".to_string()),
    })
}

pub fn render() -> Dom {
    let controls = controls();

    // A platform with nothing 30 days old yet would otherwise open on an
    // empty table, which reads as a broken page rather than a young one.
    if controls.view.get() == View::Listed
        && data::listed().is_empty()
        && !data::newest().is_empty()
    {
        controls.view.set(View::New);
    }

    html!("div", {
        .class("stack-lg")
        .child(header())
        .child(html!("div", {
            .class("stack")
            .child(view_tabs())
            .child(filters())
            .child(html!("div", {
                .child_signal(map_ref! {
                    let view = controls.view.signal(),
                    let key = controls.sort.key.signal_cloned(),
                    let descending = controls.sort.descending.signal(),
                    let chain = controls.chain.signal_cloned(),
                    let source = controls.source.signal_cloned(),
                    let exit = controls.exit.signal_cloned() => {
                        Some(listing(*view, key, *descending, chain, source, exit))
                    }
                })
            }))
        }))
        .child(rules())
    })
}

fn header() -> Dom {
    html!("div", {
        .class("stack")
        .child(html!("h1", { .class("title").text("Agents") }))
        .child(html!("p", {
            .class("lede")
            .text(
                "Every agent's return over 30 days and since opening, its deepest drop and the \
                 money in its vault, read from its own contract. Sort by whichever matters to you."
            )
        }))
    })
}

fn view_tabs() -> Dom {
    let controls = controls();
    let counts = (
        data::listed().len(),
        data::newest().len(),
        data::past().len(),
    );

    html!("div", {
        .class(["row", "row--wrap", "row--between"])
        .child(tabs::segmented(
            vec![
                (View::Listed, "Leaderboard"),
                (View::New, "New"),
                (View::Past, "Stopped"),
            ],
            &controls.view,
        ))
        .child(html!("span", {
            .class(["small", "faint", "num"])
            .text(&format!("{} listed · {} new · {} stopped", counts.0, counts.1, counts.2))
        }))
    })
}

fn filters() -> Dom {
    let controls = controls();

    html!("div", {
        .class(["row", "row--wrap"])
        .child(form::field(
            "Chain",
            None,
            form::select(
                std::iter::once(("all".to_string(), "Every chain".to_string()))
                    .chain(
                        shared::Chain::ALL
                            .map(|chain| (chain.name().to_string(), chain.label().to_string())),
                    )
                    .collect(),
                &controls.chain,
            ),
        ))
        .child(form::field(
            "Source",
            None,
            form::select(
                vec![
                    ("all".to_string(), "Published or sealed".to_string()),
                    ("published".to_string(), "Source published".to_string()),
                    ("sealed".to_string(), "Source sealed".to_string()),
                ],
                &controls.source,
            ),
        ))
        .child(form::field(
            "Exit",
            None,
            form::select(
                vec![
                    ("all".to_string(), "Any exit class".to_string()),
                    ("direct".to_string(), "Direct: needs nobody".to_string()),
                    ("served".to_string(), "Served by an unwinder".to_string()),
                ],
                &controls.exit,
            ),
        ))
        // The table's own headers sort it, but they are hidden when the table
        // collapses on a narrow screen, so sorting needs a control of its own
        // — bound to the same state, not a copy of it.
        .child(form::field(
            "Sort by",
            None,
            form::select(
                SORT_KEYS
                    .iter()
                    .map(|(key, label)| ((*key).to_string(), (*label).to_string()))
                    .collect(),
                &controls.sort.key,
            ),
        ))
    })
}

/// Every key the leaderboard can sort by, and what it is called.
const SORT_KEYS: [(&str, &str); 5] = [
    ("month", "Last 30 days"),
    ("since", "Since opening"),
    ("drop", "Deepest drop"),
    ("aum", "In the vault"),
    ("age", "Days live"),
];

fn listing(
    view: View,
    sort_key: &str,
    descending: bool,
    chain: &str,
    source: &str,
    exit: &str,
) -> Dom {
    let controls = controls();
    let mut strategies: Vec<Rc<Strategy>> = match view {
        View::Listed => data::listed(),
        View::New => data::newest(),
        View::Past => data::past(),
    }
    .into_iter()
    .filter(|strategy| matches(strategy, chain, source, exit))
    .collect();

    if view == View::Past {
        strategies.sort_by_key(|strategy| std::cmp::Reverse(strategy.registered));
        return past_table(strategies);
    }

    strategies.sort_by(|a, b| {
        let ordering = key_of(a, sort_key)
            .partial_cmp(&key_of(b, sort_key))
            .unwrap_or(std::cmp::Ordering::Equal);
        if descending {
            ordering.reverse()
        } else {
            ordering
        }
    });

    if strategies.is_empty() {
        return ui::empty("No agent matches those filters.");
    }

    let rows: Vec<Row> = strategies.iter().map(|strategy| row(strategy)).collect();
    let columns = vec![
        Column::new("Agent").wide(),
        Column::new("Author"),
        Column::num("Live").sortable("age"),
        Column::num("30 days").sortable("month"),
        Column::num("Since opening").sortable("since"),
        Column::num("Deepest drop").sortable("drop"),
        Column::num("In the vault").sortable("aum"),
        Column::new("Shape"),
    ];

    html!("div", {
        .class("stack-sm")
        .child(sortable_table(columns, rows, &controls.sort))
        .child(html!("p", {
            .class(["faint", "small"])
            .text(
                "Every figure is read from the vault's own contract: value per share for the \
                 returns and the drop, and the money it holds. Anyone can recompute them."
            )
        }))
    })
}

fn row(strategy: &Strategy) -> Row {
    let performance = data::performance(strategy);
    let cells = vec![
        html!("div", {
            .child(html!("div", { .class("table__primary").text(strategy.name()) }))
            .child(html!("div", {
                .class(["faint", "small"])
                .text(&format!("{} · {}", strategy.vault.chain.label(), strategy.slug))
            }))
        }),
        ui::muted(&strategy.author),
        ui::number(&fmt::days(strategy.days_live)),
        match performance.return_30d {
            Some(value) => ui::signed(value, &fmt::percent_signed(value, 1)),
            None => ui::muted("—"),
        },
        ui::signed(
            performance.return_since_open,
            &fmt::percent_signed(performance.return_since_open, 1),
        ),
        ui::number(&fmt::percent_signed(performance.deepest_drop, 1)),
        ui::number(&fmt::money_compact(strategy.capital())),
        parts::spark(strategy),
    ];
    Row::linked(parts::path_of(strategy), cells)
}

fn past_table(strategies: Vec<Rc<Strategy>>) -> Dom {
    if strategies.is_empty() {
        return ui::empty("Nothing has been stopped yet.");
    }

    let rows: Vec<Row> = strategies
        .into_iter()
        .map(|strategy| {
            let performance = data::performance(&strategy);
            Row::linked(
                parts::path_of(&strategy),
                vec![
                    html!("span", { .class("table__primary").text(strategy.name()) }),
                    ui::muted(&strategy.author),
                    parts::standing_badge(strategy.standing),
                    ui::number(&fmt::days(strategy.days_live)),
                    ui::number(&fmt::percent_signed(performance.return_since_open, 1)),
                    ui::muted(&reason(&strategy)),
                ],
            )
        })
        .collect();

    html!("div", {
        .class("stack-sm")
        .child(crate::ui::table::table(
            vec![
                Column::new("Agent").wide(),
                Column::new("Author"),
                Column::new("Standing"),
                Column::num("Live"),
                Column::num("Total return"),
                Column::new("What happened").wide(),
            ],
            rows,
        ))
        .child(html!("p", {
            .class(["faint", "small"])
            .text(
                "Records stay here, beside the ones still trading."
            )
        }))
    })
}

fn reason(strategy: &Strategy) -> String {
    match strategy.standing {
        Standing::Frozen => {
            "A claim went unserved, so it takes no deposits and only unwinds.".to_string()
        }
        Standing::Killed => strategy
            .kill
            .as_ref()
            .map_or_else(|| "Stopped.".to_string(), |kill| kill.reason.clone()),
        Standing::Closed => "Closed. Anyone still in withdraws from cash.".to_string(),
        _ => String::new(),
    }
}

fn key_of(strategy: &Strategy, key: &str) -> f64 {
    let performance = data::performance(strategy);
    match key {
        "since" => performance.return_since_open,
        "drop" => performance.deepest_drop,
        "age" => f64::from(strategy.days_live),
        "aum" => strategy.capital(),
        _ => performance.return_30d.unwrap_or(f64::MIN),
    }
}

fn matches(strategy: &Strategy, chain: &str, source: &str, exit: &str) -> bool {
    let chain_matches = match shared::Chain::from_name(chain) {
        Some(wanted) => strategy.vault.chain == wanted,
        None => true,
    };

    let source_matches = match source {
        "published" => matches!(strategy.bundle.source, Source::Published { .. }),
        "sealed" => matches!(strategy.bundle.source, Source::Sealed),
        _ => true,
    };

    let exit_matches = match exit {
        "direct" => strategy.vault.exit_class == ExitClass::Direct,
        "served" => matches!(strategy.vault.exit_class, ExitClass::Served { .. }),
        _ => true,
    };

    chain_matches && source_matches && exit_matches
}

fn rules() -> Dom {
    ui::section(
        "How this list is built",
        vec![html!("div", {
            .class(["grid", "grid--2"])
            .children(copy::ranking().into_iter().map(|(title, body)| {
                ui::card(vec![
                    html!("h3", { .class("subtitle").text(&title) }),
                    ui::para(&body),
                ])
            }))
        })],
    )
}
