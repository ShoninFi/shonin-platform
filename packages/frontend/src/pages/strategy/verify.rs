//! "Which code is trading this vault?" — the panel a depositor should read,
//! and the checks anyone can run against it themselves.

use std::rc::Rc;

use dominator::{Dom, html};
use shared::{KeyRole, Strategy};

use crate::copy;
use crate::data;
use crate::format as fmt;
use crate::pages::parts;
use crate::ui::primitives as ui;
use crate::ui::primitives::Tone;
use crate::ui::table::{Column, Row, table};

pub fn render(strategy: &Rc<Strategy>) -> Dom {
    let Some(attestation) = &strategy.attestation else {
        return ui::section(
            "Nothing is running",
            vec![ui::empty(
                "No keys are blessed for this strategy, so nothing can trade for it.",
            )],
        );
    };

    html!("div", {
        .class("stack-lg")
        .child(panel(strategy, attestation))
        .child(keys(attestation))
        .child(approvals(attestation))
        .child(checks())
        .child(audits(strategy))
        .child(changing())
    })
}

fn panel(strategy: &Rc<Strategy>, attestation: &shared::Attestation) -> Dom {
    let value = format!(
        "{} {}",
        fmt::money(strategy.vault.value),
        strategy.vault.asset
    );

    ui::section(
        "Which code is trading this vault?",
        vec![
            ui::note("Everything in this panel is on-chain and can be checked without asking us."),
            ui::kv(vec![
                ("Strategy".to_string(), ui::value(&strategy.name())),
                (
                    "Bundle".to_string(),
                    html!("div", {
                        .child(ui::hash(&strategy.bundle.bundle_hash))
                        .child(html!("div", {
                            .class(["faint", "small"])
                            .text(if strategy.bundle.source.is_published() {
                                "source published · rebuilds to this hash"
                            } else {
                                "source sealed · the builder compiled it and the reviewer read it"
                            })
                        }))
                    }),
                ),
                (
                    "Built by".to_string(),
                    html!("div", {
                        .child(ui::hash(&attestation.builder.digest))
                        .child(html!("div", {
                            .class(["faint", "small"])
                            .text(&format!(
                                "from source {} · allowlisted {} · audit {}",
                                fmt::short_hash(&strategy.bundle.source_hash),
                                fmt::date(attestation.builder.allowlisted),
                                attestation.builder.audit.label()
                            ))
                        }))
                    }),
                ),
                (
                    "Executor image".to_string(),
                    html!("div", {
                        .child(ui::hash(&attestation.image.digest))
                        .child(html!("div", {
                            .class(["faint", "small"])
                            .text(&format!(
                                "allowlisted {} · this vault was created {} · audit {}",
                                fmt::date(attestation.image.allowlisted),
                                fmt::date(strategy.vault.created),
                                attestation.image.audit.label()
                            ))
                        }))
                    }),
                ),
                ("Trust root".to_string(), ui::value(&attestation.trust_root)),
                (
                    "Value now".to_string(),
                    html!("div", {
                        .child(ui::number(&value))
                        .child(html!("div", {
                            .class(["faint", "small"])
                            .text("computed by the vault contract, not reported")
                        }))
                    }),
                ),
            ]),
            ui::notice(
                Tone::Accent,
                "One key, one vault, one strategy version",
                &format!(
                    "Each blessing binds a key to this vault and this bundle hash, and lasts {} hours \
                     before the executor has to attest again.",
                    attestation.blessing_ttl_hours
                ),
            ),
        ],
    )
}

fn keys(attestation: &shared::Attestation) -> Dom {
    let now = data::now();
    let rows: Vec<Row> = attestation
        .keys
        .iter()
        .map(|key| {
            let hours_left = (key.blessed_until - now).whole_hours().max(0);
            let fraction = hours_left as f64 / f64::from(attestation.blessing_ttl_hours);
            Row::new(vec![
                ui::badge(
                    key.role.label(),
                    match key.role {
                        KeyRole::Primary => Tone::Accent,
                        KeyRole::Standby => Tone::Info,
                        KeyRole::Unwinder => Tone::Neutral,
                    },
                ),
                ui::address(&key.address),
                ui::muted(&key.zone),
                html!("div", {
                    .child(ui::number(&fmt::until(key.blessed_until, now)))
                    .child(html!("div", {
                        .class(["faint", "small"])
                        .text(&fmt::timestamp(key.blessed_until))
                    }))
                }),
                ui::ring(fraction, &format!("{hours_left}h")),
                ui::hash(&key.proof_tx),
            ])
        })
        .collect();

    ui::section(
        "Blessed keys",
        vec![
            table(
                vec![
                    Column::new("Role"),
                    Column::new("Key"),
                    Column::new("Where it runs"),
                    Column::new("Blessed until").wide(),
                    Column::new("Left"),
                    Column::new("Proof"),
                ],
                rows,
            ),
            ui::note(
                "The keys are generated inside the enclave and never leave it. A standby in another \
                 zone is blessed in advance, and the chain is the handoff, so two copies never \
                 trade at once.",
            ),
        ],
    )
}

fn approvals(attestation: &shared::Attestation) -> Dom {
    let rows: Vec<Row> = attestation
        .approvals
        .iter()
        .map(|approval| {
            Row::new(vec![
                html!("span", { .class("table__primary").text(&approval.venue) }),
                ui::muted(&approval.detail),
                parts::custody_badge(approval.custody),
            ])
        })
        .collect();

    ui::section(
        "What the venues have approved",
        vec![
            table(
                vec![
                    Column::new("Venue"),
                    Column::new("Approval").wide(),
                    Column::new("Custody"),
                ],
                rows,
            ),
            ui::note(
                "Anyone can send the transaction that approves or revokes a key; the contract obeys \
                 only what the registry justifies.",
            ),
        ],
    )
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

fn audits(strategy: &Rc<Strategy>) -> Dom {
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
            ui::note(
                "A vault opens before every audit is finished, so the page says where each one \
                 stands.",
            ),
        ],
    )
}

fn changing() -> Dom {
    ui::section(
        "What can change, and what can't",
        vec![
            html!("div", {
                .class(["grid", "grid--2"])
                .child(ui::card(vec![
                    ui::subheading("The strategy never changes"),
                    ui::para(
                        "A published bundle is frozen. A new version is a new bundle with its own vault.",
                    ),
                ]))
                .child(ui::card(vec![
                    ui::subheading("The executor image can"),
                    ui::para(&format!(
                        "It is our code, and a fix has to reach running vaults. A new image is \
                         proposed publicly and blessed only after {}; removing one is instant.",
                        match data::terms() {
                            Some(terms) => format!(
                                "a timelock of {}",
                                fmt::day_count(terms.image_timelock_days)
                            ),
                            None => "a public timelock".to_string(),
                        }
                    )),
                ]))
            }),
            ui::action_link("/status", "Every allowlist change, as it happened"),
        ],
    )
}
