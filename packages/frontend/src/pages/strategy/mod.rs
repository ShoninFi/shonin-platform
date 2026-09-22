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
        .child(tabs(items, "Strategy"))
    })
}

/// The fee schedule, which both products show the same way.
pub fn fee_table() -> Dom {
    use crate::copy;
    use crate::ui::table::{Column, Row, table};

    table(
        vec![
            Column::new("Fee"),
            Column::new("Charged on").wide(),
            Column::new("Paid to"),
            Column::new("Level"),
        ],
        copy::FEES
            .iter()
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
