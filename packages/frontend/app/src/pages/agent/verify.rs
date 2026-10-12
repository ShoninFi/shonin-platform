//! `/agents/:id/verify`: which code is trading this vault. The five records
//! on the chain from the source to the vault, how to rebuild the source and
//! compare, the keys allowed to trade and the machine images behind them,
//! and what checking can't tell you.

use std::rc::Rc;

use shared::{KeyRole, Source, Strategy};
use ui::icons::Icon;

use super::parts::{self, card};
use super::{facts, live, side};
use crate::atoms::icon;
use crate::format;
use crate::route::DOCS;
use crate::store::{self, Agent};
use crate::view::{El, View, el, svg, text};

pub fn body(agent: &Rc<Agent>) -> El {
    el("div")
        .class("ag-layout")
        .class("ag-layout--tab")
        .child(
            el("div")
                .class("ag-main")
                .child(live(agent, chain_of_records))
                .child(live(agent, rebuild))
                .child(live(agent, keys))
                .child(live(agent, images))
                .child(cant_tell()),
        )
        .child(
            el("aside")
                .class("ag-side")
                .attr("aria-label", "Getting out, and every hash")
                .child(side::way_out(agent))
                .child(side::all_hashes(agent)),
        )
}

/// Whether this view should play the reveal: in the app, not in the
/// pre-rendered page (which the app replaces as it starts).
fn reveal() -> bool {
    cfg!(target_arch = "wasm32")
}

struct Node {
    title: String,
    sub: String,
    value: Option<String>,
    what: &'static str,
    glyph: Icon,
    link: Option<&'static str>,
}

fn nodes(strategy: &Strategy) -> Vec<Node> {
    let source_sub = match (&strategy.bundle.source, facts::source_size(strategy)) {
        (Source::Sealed, _) => "Sealed".to_string(),
        (_, Some((files, _))) => format!("Public · {} files", format::number(files as f64, 0)),
        _ => "Public".to_string(),
    };
    let review = strategy.review.as_ref();
    let attestation = strategy.attestation.as_ref();
    let now = store::get().as_of.get();
    let live_keys = attestation
        .map(|attestation| {
            attestation
                .keys
                .iter()
                .filter(|key| now.is_none_or(|now| key.blessed_until > now))
                .count()
        })
        .unwrap_or(0);
    vec![
        Node {
            title: "Source".into(),
            sub: source_sub,
            value: Some(strategy.bundle.source_hash.clone()),
            what: "source hash",
            glyph: Icon::Code,
            link: Some("The report on the chain names the source it read."),
        },
        Node {
            title: "Review".into(),
            sub: review
                .map(|review| format!("Reviewed {}", facts::full_day(review.reviewed)))
                .unwrap_or_else(|| "Not on the chain yet".into()),
            value: review.map(|review| review.report_hash.clone()),
            what: "report hash",
            glyph: Icon::Review,
            link: Some("The build came from that same source, on an approved builder."),
        },
        Node {
            title: "Build".into(),
            sub: format!("Built in {}s", strategy.bundle.built_in_seconds),
            value: Some(strategy.bundle.component_hash.clone()),
            what: "build hash",
            glyph: Icon::Machine,
            link: Some("Each key was made inside an attested machine running this exact build."),
        },
        Node {
            title: if attestation.is_some() {
                format!("Keys · {live_keys} blessed")
            } else {
                "Keys".into()
            },
            sub: attestation
                .map(|attestation| {
                    format!("Image {}", format::short_hash(&attestation.image.digest))
                })
                .unwrap_or_else(|| "None blessed yet".into()),
            value: attestation.map(|attestation| attestation.image.digest.clone()),
            what: "executor image",
            glyph: Icon::Seal,
            link: Some("The contract checked each key's attestation before letting it trade."),
        },
        Node {
            title: "Vault".into(),
            sub: format!("Contract on {}", strategy.vault.chain.label()),
            value: Some(strategy.vault.address.clone()),
            what: "vault contract address",
            glyph: Icon::Vault,
            link: None,
        },
    ]
}

fn chain_of_records(strategy: &Strategy) -> View {
    let nodes = nodes(strategy);
    let complete = nodes.iter().all(|node| node.value.is_some());
    let as_of = store::get()
        .chains
        .lock_ref()
        .iter()
        .find(|read| read.chain == strategy.vault.chain)
        .and_then(|read| read.as_of);
    let total = nodes.len();
    let row = el("ol")
        .class("ag-records")
        .class_if(reveal(), "ag-records--reveal")
        .children(nodes.iter().enumerate().map(|(index, node)| {
            el("li")
                .class("ag-record")
                .class_if(node.value.is_none(), "ag-record--missing")
                .child(
                    el("div")
                        .class("ag-record__top")
                        .child(
                            el("span")
                                .class("ag-record__icon")
                                .child(icon(node.glyph, 18)),
                        )
                        .child(
                            el("span")
                                .class("ag-record__n")
                                .text(format!("{} of {total}", index + 1)),
                        ),
                )
                .child(el("p").class("ag-record__title").text(node.title.clone()))
                .child(el("p").class("ag-record__sub").text(node.sub.clone()))
                .child(match &node.value {
                    Some(value) => parts::chip(value, node.what),
                    None => el("span").class("ag-record__none").text("Missing"),
                })
                .child(View::from(node.link.map(|_| {
                    el("span")
                        .class("ag-record__link")
                        .attr("aria-hidden", "true")
                        .child(
                            svg("svg")
                                .attr("viewBox", "0 0 24 24")
                                .attr("width", "20")
                                .attr("height", "20")
                                .child(
                                    svg("path")
                                        .class("ag-record__check")
                                        .attr("d", "M5 12.5l4.5 4.5L19 7.5")
                                        .attr("pathLength", "1"),
                                ),
                        )
                        .into()
                })))
        }));
    let result = if complete {
        el("div")
            .class("ag-records__result")
            .attr("role", "status")
            .child(
                el("span")
                    .class("ag-seal")
                    .class_if(reveal(), "ag-seal--reveal")
                    .child(icon(Icon::Seal, 24)),
            )
            .child(
                el("div")
                    .child(el("p").class("ag-records__verdict").text("Every link is on the chain."))
                    .child(el("p").class("ag-muted").text(
                        "The code the Reviewer read is the code that was built, and only that build's keys can trade this vault.",
                    )),
            )
    } else {
        el("div")
            .class("ag-records__result")
            .class("ag-records__result--open")
            .attr("role", "status")
            .child(el("span").class("ag-seal").class("ag-seal--open").child(icon(Icon::Clock, 24)))
            .child(
                el("div")
                    .child(el("p").class("ag-records__verdict").text("Not every link is on the chain yet."))
                    .child(el("p").class("ag-muted").text(
                        "A record marked Missing hasn't been posted for this vault. Until it is, this page can't show the whole chain from source to keys.",
                    )),
            )
    };
    el("section")
        .class("ag-verify")
        .attr("aria-labelledby", "ag-verify-title")
        .child(el("p").class("sh-label").text("Verification"))
        .child(el("h2").class("ag-verify__title").attr("id", "ag-verify-title").text("Which code is trading this vault?"))
        .child(
            el("div")
                .class("ag-verify__lede")
                .child(el("p").text(
                    "Five records on the chain, each pointing at the next. Follow them from the source the Reviewer read to the keys allowed to trade your money.",
                ))
                .child(View::from(as_of.map(|at| {
                    el("p")
                        .class("ag-verify__when")
                        .child(icon(Icon::Clock, 16))
                        .text(format!(
                            "Read from the chain {}, {}",
                            facts::full_day(at.date()),
                            format::time_short(at)
                        ))
                        .into()
                }))),
        )
        .child(row)
        .child(result)
        .child(
            el("dl")
                .class("ag-verify__links")
                .children(
                    [
                        ("Source → Review", "The report on the chain names the source it read."),
                        ("Review → Build", "The build came from that same source, on an approved builder."),
                        ("Build → Keys", "Each key was made inside an attested machine running this exact build."),
                        ("Keys → Vault", "The contract checked each key's attestation before letting it trade."),
                    ]
                    .into_iter()
                    .map(|(term, words)| el("div").child(el("dt").text(term)).child(el("dd").text(words))),
                ),
        )
        .into()
}

/// A short hash written one character at a time, so it can resolve left to
/// right (Motion M9).
fn spelled(value: &str) -> El {
    let short = format::short_hash(value);
    el("span")
        .class("ag-spell")
        .attr("title", value.to_string())
        .child(el("span").class("sh-vh").text(value.to_string()))
        .child(
            el("span")
                .attr("aria-hidden", "true")
                .class_if(reveal(), "ag-spell--reveal")
                .children(short.chars().map(|c| el("span").text(c.to_string()))),
        )
}

fn rebuild(strategy: &Strategy) -> View {
    let build = &strategy.bundle.component_hash;
    let (toolchain, target) = match strategy.bundle.toolchain.split_once(" · ") {
        Some((toolchain, target)) => (toolchain.to_string(), target.to_string()),
        None => (
            strategy.bundle.toolchain.clone(),
            format::MISSING.to_string(),
        ),
    };
    let fact = |key: &'static str, value: View| {
        el("div")
            .child(el("dt").class("sh-label").text(key))
            .child(el("dd").child(value))
    };
    if let Source::Sealed = strategy.bundle.source {
        return card("ag-rebuild-title", "Which code is trading this vault?", Some(Icon::Lock))
            .child(el("p").class("ag-body").text(
                "You can't rebuild a sealed agent. Instead, the chain records which source the Reviewer read, and that the running build came from that same source.",
            ))
            .child(
                el("dl")
                    .class("ag-facts-grid")
                    .child(fact("Toolchain", text(toolchain)))
                    .child(fact("Target", el("code").class("ag-code").text(target).into()))
                    .child(fact("Source", text("Sealed")))
                    .child(fact("Build on chain", el("span").class("sh-mono").text(format::short_hash(build)).into())),
            )
            .into();
    }
    let (rust, _) = toolchain
        .split_once(' ')
        .map(|(_, version)| (version.to_string(), ()))
        .unwrap_or((toolchain.clone(), ()));
    let size = facts::source_size(strategy)
        .map(|(files, lines)| {
            format!(
                "{} files · {} lines",
                format::number(files as f64, 0),
                format::number(lines as f64, 0)
            )
        })
        .unwrap_or_else(|| "Public".to_string());
    let where_from = match &strategy.bundle.source {
        Source::Published { repo, .. } => repo.clone(),
        Source::Sealed => String::new(),
    };
    let line = |kind: &'static str, words: String| el("span").class(kind).text(words);
    let terminal = el("div")
        .class("ag-term")
        .child(
            el("div")
                .class("ag-term__bar")
                .child(el("span").class("ag-term__name").child(icon(Icon::Code, 16)).text("Terminal")),
        )
        .child(
            el("pre").class("ag-term__body").child(
                el("code")
                    .child(line("ag-term__comment", "# 1  Get the source at the recorded commit\n".into()))
                    .child(line("ag-term__confirm", format!("[CONFIRM: source repository URL — the chain records {where_from}]")))
                    .text("\n\n")
                    .child(line("ag-term__comment", "# 2  Build with the pinned toolchain\n".into()))
                    .text(format!("rustup toolchain install {rust} --target {target}\ncargo build --release --locked --target {target}\n\n"))
                    .child(line("ag-term__comment", "# 3  Compare the fingerprint\n".into()))
                    .child(line("ag-term__confirm", "[CONFIRM: fingerprint command]".into()))
                    .text("\n")
                    .child(line("ag-term__comment", format!("# expect {}", format::short_hash(build)))),
            ),
        );
    card("ag-rebuild-title", "Rebuild it yourself", None)
        .child(el("p").class("ag-body").text(
            "Builds are exactly repeatable. Build the source with the published recipe and you get the same fingerprint the chain holds. You don't have to trust us, or the page you're reading.",
        ))
        .child(
            el("dl")
                .class("ag-facts-grid")
                .child(fact("Toolchain", text(toolchain.clone())))
                .child(fact("Target", el("code").class("ag-code").text(target.clone()).into()))
                .child(fact("Source", text(size)))
                .child(fact("Build on chain", el("span").class("sh-mono").text(format::short_hash(build)).into())),
        )
        .child(
            el("ol")
                .class("ag-rsteps")
                .child(el("li").child(el("strong").text("Get the source")).text(format!(
                    " at the commit the chain records for {}.",
                    format::short_hash(&strategy.bundle.source_hash)
                )))
                .child(el("li").child(el("strong").text("Build it")).text(" with the pinned toolchain and locked dependencies."))
                .child(el("li").child(el("strong").text("Compare")).text(" your fingerprint with the build the chain holds.")),
        )
        .child(terminal)
        .child(
            el("div")
                .class("ag-match")
                .child(
                    el("div")
                        .class("ag-match__box")
                        .child(el("p").class("sh-label").text("The chain holds"))
                        .child(spelled(build)),
                )
                .child(el("span").class("ag-match__eq").attr("aria-hidden", "true").text("="))
                .child(
                    el("div")
                        .class("ag-match__box")
                        .class("ag-match__box--yours")
                        .child(el("p").class("sh-label").text("You should get"))
                        .child(spelled(build)),
                ),
        )
        .child(
            el("p")
                .class("ag-muted")
                .text("If yours is different, the build on the chain isn't the one you made. Don't rely on it, and tell us on the ")
                .child(crate::web::app_link("/status").text("status page"))
                .text(". ")
                .child(el("a").attr("href", format!("{DOCS}verify/")).text("The full rebuild guide")),
        )
        .into()
}

fn keys(strategy: &Strategy) -> View {
    let Some(attestation) = &strategy.attestation else {
        return card("ag-keys-title", "The keys trading this vault", None)
            .child(el("p").class("ag-body").text(
                "No executor key is blessed for this vault yet. Only keys made inside an attested machine running this exact build can trade, and none has been recorded.",
            ))
            .into();
    };
    let now = store::get().as_of.get();
    let this_year = now.map(|at| at.year()).unwrap_or(2026);
    let chain = strategy.vault.chain;
    let mut keys: Vec<&shared::BlessedKey> = attestation.keys.iter().collect();
    keys.sort_by_key(|key| std::cmp::Reverse(key.blessed_until));
    let role = |role: KeyRole| match role {
        KeyRole::Primary => "Primary",
        KeyRole::Standby => "Standby",
        KeyRole::Unwinder => "Unwinder",
    };
    card("ag-keys-title", "The keys trading this vault", None)
        .class("ag-card--flush-table")
        .child(el("p").class("ag-body").text(
            "Only keys made inside an attested machine running this exact build can trade. They can place and cancel orders, and nothing else.",
        ))
        .child(
            el("div").class("ag-scroll").child(
                el("table")
                    .class("sh-table")
                    .class("ag-keys")
                    .child(el("caption").class("sh-vh").text("Blessed keys"))
                    .child(
                        el("thead").child(
                            el("tr")
                                .child(el("th").attr("scope", "col").text("Key"))
                                .child(el("th").attr("scope", "col").text("Made on image"))
                                .child(el("th").attr("scope", "col").text("Expires"))
                                .child(el("th").attr("scope", "col").text("Proof")),
                        ),
                    )
                    .child(el("tbody").children(keys.into_iter().map(|key| {
                        let expired = now.is_some_and(|now| key.blessed_until <= now);
                        let left = now
                            .filter(|_| !expired)
                            .map(|now| {
                                let days = crate::model::days_until(key.blessed_until, now);
                                format!("in {}", format::days(u32::try_from(days).unwrap_or(0)))
                            })
                            .unwrap_or_else(|| "Expired".to_string());
                        el("tr")
                            .class("sh-row")
                            .class_if(expired, "ag-keys__expired")
                            .child(
                                el("th")
                                    .attr("scope", "row")
                                    .child(el("span").class("sh-mono").attr("title", key.address.clone()).text(format::short_hash(&key.address)))
                                    .child(el("span").class("ag-keys__role").text(format!("{} · trade-only", role(key.role)))),
                            )
                            .child(el("td").child(el("span").class("sh-mono").text(format::short_hash(&attestation.image.digest))))
                            .child(
                                el("td")
                                    .child(el("span").class("ag-keys__day").text(format!(
                                        "{}, {}",
                                        format::day(key.blessed_until.date(), this_year),
                                        format::time_short(key.blessed_until)
                                    )))
                                    .child(el("span").class("ag-keys__left").text(left)),
                            )
                            .child(el("td").child(parts::tx_link(chain, &key.proof_tx, "Proof transaction")))
                    }))),
            ),
        )
        .child(
            el("div")
                .class("ag-keys__notes")
                .child(
                    el("p")
                        .child(icon(Icon::Seal, 16))
                        .child(el("span").child(el("strong").text("A blessing is narrow.")).text(format!(
                            " One key, made on one image, may act for one vault running one build, for {} at a time. It's renewed by attesting again.",
                            if attestation.blessing_ttl_hours % 24 == 0 {
                                format::days(attestation.blessing_ttl_hours / 24)
                            } else {
                                format!("{} hours", attestation.blessing_ttl_hours)
                            }
                        ))),
                )
                .child(
                    el("p")
                        .child(icon(Icon::Machine, 16))
                        .child(el("span").child(el("strong").text("The contract checks first.")).text(
                            " Before accepting a key, it checks an attestation of the machine that made it.",
                        )),
                ),
        )
        .into()
}

fn images(strategy: &Strategy) -> View {
    let mut rows: Vec<(&'static str, &'static str, String, Option<time::Date>)> = Vec::new();
    if let Some(review) = &strategy.review {
        rows.push((
            "Reviewer",
            "Reads the code, writes the report",
            review.reviewer_image.clone(),
            None,
        ));
    }
    if let Some(attestation) = &strategy.attestation {
        rows.push((
            "Builder",
            "Builds the agent from its source",
            attestation.builder.digest.clone(),
            Some(attestation.builder.allowlisted),
        ));
        rows.push((
            "Executor",
            "Runs the agent, makes its keys",
            attestation.image.digest.clone(),
            Some(attestation.image.allowlisted),
        ));
    } else {
        rows.push((
            "Builder",
            "Builds the agent from its source",
            strategy.bundle.builder_image.clone(),
            None,
        ));
    }
    let days = store::get().image_timelock_days.get();
    let wait = match days {
        Some(days) if days >= 1.0 => format!(
            " A new image only counts after a public {}-day wait, so everyone sees it coming.",
            days.round() as u32
        ),
        Some(_) => " On this network a new image counts as soon as it's on the list.".to_string(),
        None => {
            " A new image only counts after a public wait, so everyone sees it coming.".to_string()
        }
    };
    card("ag-images-title", "Approved machine images", None)
        .class("ag-card--flush-table")
        .child(el("p").class("ag-body").text(format!(
            "Only machines running an image on this public list can review, build, run or unwind agents.{wait}"
        )))
        .child(
            el("div").class("ag-scroll").child(
                el("table")
                    .class("sh-table")
                    .child(el("caption").class("sh-vh").text("Machine images behind this agent"))
                    .child(
                        el("thead").child(
                            el("tr")
                                .child(el("th").attr("scope", "col").text("Image"))
                                .child(el("th").attr("scope", "col").text("Its job"))
                                .child(el("th").attr("scope", "col").text("Fingerprint"))
                                .child(el("th").attr("scope", "col").text("Counted since")),
                        ),
                    )
                    .child(el("tbody").children(rows.into_iter().map(|(name, job, digest, since)| {
                        el("tr")
                            .class("sh-row")
                            .child(el("th").attr("scope", "row").text(name))
                            .child(el("td").class("ag-muted").text(job))
                            .child(el("td").child(el("span").class("sh-mono").attr("title", digest.clone()).text(format::short_hash(&digest))))
                            .child(el("td").text(since.map(facts::full_day).unwrap_or_else(|| format::MISSING.to_string())))
                    }))),
            ),
        )
        .child(
            el("p")
                .class("ag-note")
                .text("A newer image doesn't change a report or a build that's already on the chain. Every proposed image appears on the ")
                .child(crate::web::app_link("/status").text("status page"))
                .text(" the day it's proposed."),
        )
        .into()
}

fn cant_tell() -> El {
    let items = [
        (
            "Whether the agent is any good.",
            "Verified code can still lose money. The agent can lose money, and nothing here stops that.",
        ),
        (
            "Why a trade happened.",
            "The record shows what it traded and when. It doesn't show the reasons.",
        ),
        (
            "That our own code is right.",
            "Someone with a stolen admin key could push a dishonest image after the public wait. It couldn't withdraw your money, but it could trade it away.",
        ),
        (
            "That the hardware is honest.",
            "An attestation is only as good as the signature and the chip security behind it. If either is broken, so is the proof.",
        ),
        (
            "What happens next.",
            "Every link checks out today. That says nothing about how the agent trades tomorrow.",
        ),
    ];
    el("section")
        .class("ag-cant")
        .attr("aria-labelledby", "ag-cant-title")
        .child(
            el("h2")
                .class("ag-section-title")
                .attr("id", "ag-cant-title")
                .text("What checking can't tell you"),
        )
        .child(el("p").class("ag-muted").text(
            "Checking proves which code runs, and that nobody swapped it. That's all it proves.",
        ))
        .child(el("ol").class("sh-card").class("ag-cant__list").children(
            items.into_iter().enumerate().map(|(index, (lead, rest))| {
                el("li")
                    .child(el("span").class("ag-cant__n").text((index + 1).to_string()))
                    .child(
                        el("div")
                            .child(el("p").class("ag-cant__lead").text(lead))
                            .child(el("p").class("ag-muted").text(rest)),
                    )
            }),
        ))
}
