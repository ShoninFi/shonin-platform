//! The leaderboard: every strategy with a vault, and the rules the list is
//! built by.

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
    Ranked,
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
        view: Mutable::new(View::Ranked),
        sort: Sort::new("normalised"),
        chain: Mutable::new("all".to_string()),
        source: Mutable::new("all".to_string()),
        exit: Mutable::new("all".to_string()),
    })
}

pub fn render() -> Dom {
    let controls = controls();

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
        .child(html!("h1", { .class("title").text("Strategies") }))
        .child(html!("p", {
            .class("lede")
            .text(
                "Ranked on live on-chain records, rescaled to one level of risk, with the size \
                 each record was earned at."
            )
        }))
    })
}

fn view_tabs() -> Dom {
    let controls = controls();
    let counts = (
        data::ranked().len(),
        data::newest().len(),
        data::past().len(),
    );

    html!("div", {
        .class(["row", "row--wrap", "row--between"])
        .child(tabs::segmented(
            vec![
                (View::Ranked, "Ranked"),
                (View::New, "New"),
                (View::Past, "Superseded & stopped"),
            ],
            &controls.view,
        ))
        .child(html!("span", {
            .class(["small", "faint", "num"])
            .text(&format!("{} ranked · {} new · {} past", counts.0, counts.1, counts.2))
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
                vec![
                    ("all".to_string(), "Every chain".to_string()),
                    ("hyperevm".to_string(), "HyperEVM".to_string()),
                    ("robinhood".to_string(), "Robinhood Chain".to_string()),
                    ("solana".to_string(), "Solana".to_string()),
                ],
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
const SORT_KEYS: [(&str, &str); 7] = [
    ("normalised", "Normalised return"),
    ("raw", "Raw return"),
    ("drawdown", "Max drawdown"),
    ("score", "Review score"),
    ("aum", "Size now"),
    ("average", "Average size"),
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
    let mut strategies: Vec<&'static Strategy> = match view {
        View::Ranked => data::ranked(),
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
        return ui::empty("No strategy matches those filters.");
    }

    let ranked = view == View::Ranked;
    let rows: Vec<Row> = strategies
        .into_iter()
        .enumerate()
        .map(|(index, strategy)| row(index, strategy, ranked))
        .collect();

    let mut columns: Vec<Column> = Vec::new();
    if ranked {
        columns.push(Column::num("#"));
    }
    columns.extend([
        Column::new("Strategy").wide(),
        Column::new("Author"),
        Column::num("Live").sortable("age"),
        Column::num("Normalised").sortable("normalised"),
        Column::num("Raw").sortable("raw"),
        Column::num("Max DD").sortable("drawdown"),
        Column::num("Review").sortable("score"),
        Column::num("Avg size").sortable("average"),
        Column::num("Size now").sortable("aum"),
        Column::new("Shape"),
    ]);

    html!("div", {
        .class("stack-sm")
        .child(sortable_table(columns, rows, &controls.sort))
        .child(html!("p", {
            .class(["faint", "small"])
            .text(
                "Normalised to 15% annualised volatility, estimated only from the days before each \
                 one. Raw returns are what the vault actually did."
            )
        }))
    })
}

fn row(index: usize, strategy: &'static Strategy, ranked: bool) -> Row {
    let performance = data::performance(strategy);
    let average = strategy
        .record
        .as_ref()
        .map_or(0.0, |record| record.average_aum);

    let mut cells = Vec::new();
    if ranked {
        cells.push(ui::number(&(index + 1).to_string()));
    }
    cells.extend([
        html!("div", {
            .child(html!("div", { .class("table__primary").text(&strategy.name()) }))
            .child(html!("div", {
                .class(["faint", "small"])
                .text(&strategy.title)
            }))
        }),
        ui::muted(&strategy.author),
        ui::number(&fmt::days(strategy.days_live)),
        ui::signed(
            performance.normalised_annualised,
            &fmt::percent_signed(performance.normalised_annualised, 1),
        ),
        ui::number(&fmt::percent_signed(performance.raw_annualised, 1)),
        ui::number(&fmt::percent_signed(performance.max_drawdown, 1)),
        parts::review_badge(strategy),
        ui::number(&fmt::money_compact(average)),
        ui::number(&fmt::money_compact(strategy.capital())),
        parts::spark(strategy),
    ]);

    Row::linked(parts::path_of(strategy), cells)
}

fn past_table(strategies: Vec<&'static Strategy>) -> Dom {
    if strategies.is_empty() {
        return ui::empty("Nothing has been superseded or stopped yet.");
    }

    let rows: Vec<Row> = strategies
        .into_iter()
        .map(|strategy| {
            let performance = data::performance(strategy);
            Row::linked(
                parts::path_of(strategy),
                vec![
                    html!("span", { .class("table__primary").text(&strategy.name()) }),
                    ui::muted(&strategy.author),
                    parts::standing_badge(strategy.standing),
                    ui::number(&fmt::days(strategy.days_live)),
                    ui::number(&fmt::percent_signed(performance.total_return, 1)),
                    ui::muted(&reason(strategy)),
                ],
            )
        })
        .collect();

    html!("div", {
        .class("stack-sm")
        .child(crate::ui::table::table(
            vec![
                Column::new("Strategy").wide(),
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
                "Records stay here, and never adjust a live strategy's rank."
            )
        }))
    })
}

fn reason(strategy: &Strategy) -> String {
    match strategy.standing {
        Standing::Superseded => strategy.superseded_by.as_ref().map_or_else(
            || "Superseded.".to_string(),
            |slug| format!("Superseded by {slug}. This vault runs on unchanged."),
        ),
        Standing::Retired => "Retired by its author. The record stays.".to_string(),
        Standing::Killed => strategy
            .kill
            .as_ref()
            .map_or_else(|| "Stopped.".to_string(), |kill| kill.reason.clone()),
        Standing::BuildFailed => {
            "The build didn't produce the same bytes twice, so there is no bundle and no vault."
                .to_string()
        }
        _ => String::new(),
    }
}

fn key_of(strategy: &Strategy, key: &str) -> f64 {
    let performance = data::performance(strategy);
    match key {
        "raw" => performance.raw_annualised,
        "drawdown" => performance.max_drawdown,
        "age" => f64::from(strategy.days_live),
        "score" => strategy
            .review
            .as_ref()
            .map_or(-1.0, |review| f64::from(review.score)),
        "aum" => strategy.capital(),
        "average" => strategy
            .record
            .as_ref()
            .map_or(0.0, |record| record.average_aum),
        _ => performance.normalised_annualised,
    }
}

fn matches(strategy: &Strategy, chain: &str, source: &str, exit: &str) -> bool {
    let chain_matches = match chain {
        "hyperevm" => matches!(strategy.vault.chain, shared::Chain::HyperEvm),
        "robinhood" => matches!(strategy.vault.chain, shared::Chain::RobinhoodChain),
        "solana" => matches!(strategy.vault.chain, shared::Chain::Solana),
        _ => true,
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
            .children(copy::RANKING.iter().map(|(title, body)| {
                ui::card(vec![
                    html!("h3", { .class("subtitle").text(title) }),
                    ui::para(body),
                ])
            }))
        })],
    )
}
