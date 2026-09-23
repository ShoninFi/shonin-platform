//! A strategy's page: the header every tab shares, and the tabs themselves.

pub mod code;
pub mod health;
pub mod overview;
pub mod record;
pub mod review;
pub mod verify;

use dominator::{Dom, html};
use shared::Strategy;

use crate::data;
use crate::pages::parts;
use crate::route::{Route, Tab};
use crate::ui::primitives as ui;
use crate::ui::tabs::{TabItem, tabs};

pub fn render(slug: &str, tab: Tab) -> Dom {
    let Some(strategy) = data::strategy(slug) else {
        return crate::pages::not_found::render();
    };

    html!("div", {
        .class("stack-lg")
        .child(header(&strategy, tab))
        .child(match tab {
            Tab::Overview => overview::render(&strategy),
            Tab::Record => record::render(&strategy),
            Tab::Code => code::render(&strategy),
            Tab::Review => review::render(&strategy),
            Tab::Verify => verify::render(&strategy),
            Tab::Health => health::render(&strategy),
        })
    })
}

fn header(strategy: &Strategy, current: Tab) -> Dom {
    let items = Tab::ALL
        .into_iter()
        .map(|tab| TabItem {
            path: Route::Strategy {
                slug: strategy.slug.clone(),
                tab,
            }
            .path()
            .unwrap_or_default(),
            label: tab.short_label(),
            current: tab == current,
        })
        .collect();

    html!("div", {
        .class("stack")
        .child(html!("div", {
            .class(["row", "row--wrap", "row--between", "row--top"])
            .child(html!("div", {
                .child(html!("h1", { .class("title").text(&strategy.name()) }))
                .child(parts::byline(strategy))
            }))
            .child(parts::labels(strategy))
        }))
        .children(parts::vault_notices(strategy))
        .child(tabs(items, "Agent"))
    })
}

/// The fee schedule, which both products show the same way.
/// A vault's fee schedule, read from the vault. It never changes under a
/// running vault, so what a page shows is what that vault charges rather than
/// what the platform charges today.
pub fn fee_table(fees: &shared::Fees) -> Dom {
    use crate::format as fmt;
    use crate::ui::table::{Column, Row, table};

    let rows = [
        (
            "Streaming fee",
            "The vault's value",
            "Shonin",
            format!("{} a year", fmt::from_bps(fees.streaming_bps)),
        ),
        (
            "Author fee",
            "Gains above the high-water mark, daily",
            "The author, filling the vault's tank first",
            format!("{} of profit", fmt::from_bps(fees.author_bps)),
        ),
        (
            "Shonin fee",
            "The same gains",
            "Shonin",
            format!("{} of profit", fmt::from_bps(fees.platform_bps)),
        ),
        (
            "Transaction fee",
            "Every deposit and withdrawal",
            "Nobody: it stays in the pool",
            fmt::from_bps(fees.transaction_bps),
        ),
    ];

    table(
        vec![
            Column::new("Fee"),
            Column::new("Charged on").wide(),
            Column::new("Paid to"),
            Column::new("Level"),
        ],
        rows.iter()
            .map(|(fee, on, to, level)| {
                Row::new(vec![
                    ui::value(fee),
                    ui::muted(on),
                    ui::muted(to),
                    ui::number(level),
                ])
            })
            .collect(),
    )
}
