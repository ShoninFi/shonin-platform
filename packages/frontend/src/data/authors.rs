//! The authors publishing on the platform.

use shared::Author;

use super::days_ago;

pub fn all() -> Vec<Author> {
    vec![
        Author {
            handle: "shonin".to_string(),
            joined: days_ago(260),
            blurb: "Reference strategies, published through the same queue, the same review \
                    and the same vault contracts as everyone else's."
                .to_string(),
            links: vec![("Verification guide".to_string(), "/verify".to_string())],
        },
        Author {
            handle: "kestrel.lab".to_string(),
            joined: days_ago(247),
            blurb: "Two people and a funding-rate spreadsheet. We trade carry where the \
                    mechanism that usually closes it is switched off on a schedule."
                .to_string(),
            links: vec![],
        },
        Author {
            handle: "tidewater".to_string(),
            joined: days_ago(233),
            blurb: "Market making on perpetuals. Quotes are only as good as the inventory \
                    behind them, which is the whole reason we're here."
                .to_string(),
            links: vec![],
        },
        Author {
            handle: "northwind".to_string(),
            joined: days_ago(228),
            blurb: "Mirrors and momentum. Most of what we publish doesn't work; all of it is \
                    on this page."
                .to_string(),
            links: vec![],
        },
        Author {
            handle: "halcyon.works".to_string(),
            joined: days_ago(96),
            blurb: "Event-driven equities. Our source is sealed and our record isn't.".to_string(),
            links: vec![],
        },
        Author {
            handle: "moraine".to_string(),
            joined: days_ago(74),
            blurb: "Game theory with real stakes. A match escrow is margin at a venue that \
                    happens to be a game."
                .to_string(),
            links: vec![],
        },
        Author {
            handle: "pallas.qt".to_string(),
            joined: days_ago(201),
            blurb: "Agents for accounts their owners keep. Same code, same review, one record \
                    across every account running it."
                .to_string(),
            links: vec![],
        },
    ]
}
