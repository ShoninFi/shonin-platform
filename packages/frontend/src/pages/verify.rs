//! How anyone checks what the platform claims, without asking us for
//! anything.

use dominator::{Dom, html};

use crate::copy;
use crate::data;
use crate::format as fmt;
use crate::ui::primitives as ui;
use crate::ui::primitives::Tone;
use crate::ui::table::{Column, Row, table};

pub fn render() -> Dom {
    html!("div", {
        .class("stack-lg")
        .child(html!("div", {
            .class("stack")
            .child(html!("h1", { .class("title").text("Verify") }))
            .child(html!("p", {
                .class("lede")
                .text(
                    "Everything the platform claims about a vault is something you can check yourself, \
                     with a chain RPC and the published source."
                )
            }))
        }))
        .child(checks())
        .child(images())
        .child(audits())
        .child(changing())
    })
}

fn checks() -> Dom {
    let rows: Vec<Row> = copy::VERIFY_CHECKS
        .iter()
        .map(|(claim, how, needs)| {
            Row::new(vec![
                html!("span", { .class("table__primary").text(claim) }),
                ui::muted(how),
                ui::badge(needs, Tone::Neutral),
            ])
        })
        .collect();

    ui::section(
        "What anyone can check",
        vec![
            table(
                vec![
                    Column::new("Claim").wide(),
                    Column::new("How to check it").wide(),
                    Column::new("What you need"),
                ],
                rows,
            ),
            ui::notice(Tone::Info, "The limit of it", copy::VERIFY_LIMIT),
        ],
    )
}

fn images() -> Dom {
    let allowlisted: Vec<Row> = data::vaults()
        .iter()
        .filter_map(|strategy| strategy.attestation.as_ref())
        .flat_map(|attestation| [&attestation.image, &attestation.builder])
        .fold(Vec::new(), |mut seen: Vec<&shared::Image>, image| {
            if !seen.iter().any(|held| held.digest == image.digest) {
                seen.push(image);
            }
            seen
        })
        .into_iter()
        .map(|image| {
            Row::new(vec![
                ui::badge(&image.kind, Tone::Accent),
                ui::hash(&image.digest),
                ui::value(&fmt::date(image.allowlisted)),
                ui::badge(image.audit.label(), Tone::from_audit(image.audit)),
            ])
        })
        .collect();

    ui::section(
        "The images that may run",
        vec![
            table(
                vec![
                    Column::new("Kind"),
                    Column::new("Digest").wide(),
                    Column::new("Allowlisted"),
                    Column::new("Audit"),
                ],
                allowlisted,
            ),
            ui::note(
                "An image is blessed only after its timelock, during which anyone can rebuild it and \
                 compare the digest. Revoking one is instant.",
            ),
            ui::action_link("/status", "Every proposal and revocation, as it happened"),
        ],
    )
}

fn audits() -> Dom {
    let vaults = data::vaults();
    let Some(strategy) = vaults.first() else {
        return html!("span");
    };

    let rows: Vec<Row> = strategy
        .vault
        .audits
        .iter()
        .map(|audit| {
            Row::new(vec![
                html!("span", { .class("table__primary").text(&audit.subject) }),
                ui::badge(audit.status.label(), Tone::from_audit(audit.status)),
                ui::muted(&audit.note),
            ])
        })
        .collect();

    ui::section(
        "Audit status",
        vec![
            table(
                vec![
                    Column::new("What"),
                    Column::new("Status"),
                    Column::new("Notes").wide(),
                ],
                rows,
            ),
            ui::notice(
                Tone::Warn,
                "Vaults open before every audit is finished",
                "Every vault page shows where each audit stands, so a depositor can size their own \
                 exposure.",
            ),
        ],
    )
}

fn changing() -> Dom {
    ui::section(
        "What can change under a running vault",
        vec![html!("div", {
            .class(["grid", "grid--2"])
            .child(ui::card(vec![
                ui::subheading("The strategy: nothing"),
                ui::para(
                    "A published bundle is frozen. A new version is a new bundle with its own vault, \
                     which a depositor chooses to move to.",
                ),
            ]))
            .child(ui::card(vec![
                ui::subheading("The executor image: deliberately"),
                ui::para(
                    "It is our code, and a fix has to be able to reach a running vault. What bounds that \
                     is the two-day timelock, a reproducible image, and instant revocation.",
                ),
            ]))
        })],
    )
}
