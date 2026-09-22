//! The no-JavaScript view of every route, written into each page at build
//! time by `src/bin/seo.rs`.
//!
//! It renders the same data the app renders, from the same modules, in plainer
//! markup: headings, tables and links, with none of the interaction. Crawlers,
//! link previews and anyone without JavaScript see the real page; the app
//! replaces it with the interactive version when it boots.

use shared::Standing;

use crate::copy;
use crate::data;
use crate::format as fmt;
use crate::route::{Route, Tab};

// ------------------------------------------------------------- html helpers

fn esc(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn el(tag: &str, attrs: &str, inner: &str) -> String {
    if attrs.is_empty() {
        format!("<{tag}>{inner}</{tag}>")
    } else {
        format!("<{tag} {attrs}>{inner}</{tag}>")
    }
}

fn link(href: &str, text: &str) -> String {
    format!("<a href=\"{}\">{}</a>", esc(href), esc(text))
}

fn para(text: &str) -> String {
    el("p", "", &esc(text))
}

fn heading(level: u8, text: &str) -> String {
    el(&format!("h{level}"), "", &esc(text))
}

fn list(items: impl IntoIterator<Item = String>) -> String {
    let body: String = items.into_iter().map(|item| el("li", "", &item)).collect();
    el("ul", "", &body)
}

/// A table of already-escaped cells.
fn table(headers: &[&str], rows: &[Vec<String>]) -> String {
    let head: String = headers
        .iter()
        .map(|header| el("th", "scope=\"col\"", &esc(header)))
        .collect();
    let body: String = rows
        .iter()
        .map(|row| {
            let cells: String = row.iter().map(|cell| el("td", "", cell)).collect();
            el("tr", "", &cells)
        })
        .collect();
    el(
        "div",
        "class=\"table-wrap\"",
        &el(
            "table",
            "class=\"table table--static\"",
            &format!(
                "{}{}",
                el("thead", "", &el("tr", "", &head)),
                el("tbody", "", &body)
            ),
        ),
    )
}

/// A definition list of label and value pairs.
fn facts(pairs: &[(String, String)]) -> String {
    let body: String = pairs
        .iter()
        .map(|(key, value)| format!("{}{}", el("dt", "", &esc(key)), el("dd", "", value)))
        .collect();
    el("dl", "class=\"kv\"", &body)
}

fn section(title: &str, inner: &str) -> String {
    el(
        "section",
        "class=\"stack\"",
        &format!("{}{inner}", heading(2, title)),
    )
}

fn code_block(body: &str) -> String {
    el("pre", "class=\"code\"", &esc(body))
}

// ------------------------------------------------------------------- shell

/// The whole page body for a route: the header, the page, and the footer.
pub fn body(route: &Route) -> String {
    format!(
        "<a class=\"skip-link\" href=\"#main\">Skip to the page</a>{}{}{}",
        masthead(route),
        el(
            "main",
            "id=\"main\" class=\"app__main\"",
            &el("div", "class=\"page stack-lg\"", &page(route))
        ),
        colophon()
    )
}

fn masthead(route: &Route) -> String {
    let section = route.section();
    let nav: String = Route::nav()
        .into_iter()
        .map(|item| {
            let current = if Some(&item.route) == section.as_ref() {
                " aria-current=\"page\""
            } else {
                ""
            };
            let path = item.route.path().unwrap_or_else(|| "/".to_string());
            format!(
                "<a class=\"mainnav__link\" href=\"{}\"{current}>{}</a>",
                esc(&path),
                esc(item.label)
            )
        })
        .collect();

    el(
        "header",
        "class=\"masthead\"",
        &el(
            "div",
            "class=\"page masthead__inner\"",
            &format!(
                "<a class=\"brand\" href=\"/\"><img class=\"brand__mark\" src=\"/public/logo.svg\" \
                 alt=\"\" /><span class=\"brand__word\">Shonin</span></a>{}",
                el("nav", "class=\"mainnav\" aria-label=\"Primary\"", &nav)
            ),
        ),
    )
}

fn colophon() -> String {
    let links: String = [
        ("/about", "About"),
        ("/verify", "Verify"),
        ("/status", "Status"),
        ("/publish", "Publish a strategy"),
    ]
    .into_iter()
    .map(|(href, label)| link(href, label))
    .collect::<Vec<_>>()
    .join("");

    el(
        "footer",
        "class=\"colophon\"",
        &el(
            "div",
            "class=\"page colophon__inner\"",
            &format!(
                "{}{}",
                el("div", "class=\"colophon__links\"", &links),
                el("div", "", &esc(copy::PITCH))
            ),
        ),
    )
}

// -------------------------------------------------------------- the pages

fn page(route: &Route) -> String {
    match route {
        Route::Home => home(),
        Route::Strategies => strategies(),
        Route::Strategy { slug, tab } => strategy(slug, *tab),
        Route::Author { handle } => author(handle),
        Route::Portfolio => portfolio(),
        Route::Queue => queue(),
        Route::Job { id } => job(id),
        Route::Publish => publish(),
        Route::Status => status(),
        Route::Verify => verify(),
        Route::About => about(),
        Route::NotFound => not_found(),
    }
}

fn home() -> String {
    let guarantees = list(copy::GUARANTEES.iter().map(|(title, body, rules_out)| {
        format!(
            "{} {} <span class=\"faint\">{}</span>",
            el("strong", "", &esc(title)),
            esc(body),
            esc(rules_out)
        )
    }));

    let steps = list(
        copy::HOW_IT_WORKS
            .iter()
            .map(|(title, body)| format!("{} {}", el("strong", "", &esc(title)), esc(body))),
    );

    let rows: Vec<Vec<String>> = ranked()
        .into_iter()
        .take(5)
        .map(|strategy| {
            let performance = performance(strategy);
            vec![
                link(&format!("/strategies/{}", strategy.slug), &strategy.name()),
                esc(&strategy.author),
                esc(&fmt::annualised(performance.normalised_annualised)),
                esc(&fmt::money_compact(strategy.capital())),
            ]
        })
        .collect();

    let totals = facts(&[
        (
            "In vaults".to_string(),
            esc(&format!(
                "{} across {} live strategies",
                fmt::money_compact(data::total_vault_capital()),
                data::vaults()
                    .iter()
                    .filter(|strategy| strategy.standing.is_live())
                    .count()
            )),
        ),
        (
            "Trades on the record".to_string(),
            esc(&format!(
                "{}, every one of them public",
                fmt::count(data::total_trades())
            )),
        ),
        (
            "Ranked".to_string(),
            esc(&format!(
                "{}, and {} more labelled new",
                data::ranked().len(),
                data::newest().len()
            )),
        ),
    ]);

    [
        el(
            "div",
            "class=\"stack\"",
            &format!(
                "{}{}{}",
                heading(1, copy::PITCH),
                para(copy::HOME_LEDE),
                para(copy::HALT_NEVER_STEAL)
            ),
        ),
        totals,
        section("What a depositor is promised", &guarantees),
        section("How it works", &steps),
        section(
            "The strategies",
            &format!(
                "{}{}",
                table(
                    &["Strategy", "Author", "Normalised return", "Capital"],
                    &rows
                ),
                para_link("/strategies", "Every strategy, ranked")
            ),
        ),
    ]
    .concat()
}

fn para_link(href: &str, text: &str) -> String {
    el("p", "", &link(href, text))
}

use data::{performance, ranked};

fn strategies() -> String {
    let row = |strategy: &shared::Strategy| {
        let performance = performance(strategy);
        vec![
            link(&format!("/strategies/{}", strategy.slug), &strategy.name()),
            link(&format!("/authors/{}", strategy.author), &strategy.author),
            esc(&fmt::days(strategy.days_live)),
            esc(&fmt::annualised(performance.normalised_annualised)),
            esc(&fmt::annualised(performance.raw_annualised)),
            esc(&fmt::percent_signed(performance.max_drawdown, 1)),
            strategy
                .review
                .as_ref()
                .map_or_else(|| "pending".to_string(), |review| review.score.to_string()),
            esc(&fmt::money_compact(strategy.capital())),
        ]
    };

    let headers = [
        "Strategy",
        "Author",
        "Live",
        "Normalised return",
        "Raw return",
        "Max drawdown",
        "Review",
        "Capital",
    ];

    let ranked_rows: Vec<Vec<String>> = ranked().into_iter().map(row).collect();
    let new_rows: Vec<Vec<String>> = data::vaults()
        .into_iter()
        .filter(|strategy| strategy.standing == Standing::New)
        .map(row)
        .collect();
    let rest_rows: Vec<Vec<String>> = data::vaults()
        .into_iter()
        .filter(|strategy| !strategy.standing.is_live())
        .map(|strategy| {
            vec![
                link(&format!("/strategies/{}", strategy.slug), &strategy.name()),
                link(&format!("/authors/{}", strategy.author), &strategy.author),
                esc(strategy.standing.label()),
                esc(&fmt::days(strategy.days_live)),
            ]
        })
        .collect();

    let rules = list(
        copy::RANKING
            .iter()
            .map(|(title, body)| format!("{} {}", el("strong", "", &esc(title)), esc(body))),
    );

    [
        heading(1, "Strategies"),
        para(
            "Ranked on live on-chain records, rescaled to one level of risk, with the size each \
             record was earned at.",
        ),
        section("Ranked", &table(&headers, &ranked_rows)),
        section(
            "New",
            &format!(
                "{}{}",
                para("Open for deposits, and not ranked until they have ninety live days."),
                table(&headers, &new_rows)
            ),
        ),
        section(
            "Superseded, retired and stopped",
            &table(&["Strategy", "Author", "Standing", "Live"], &rest_rows),
        ),
        section("How this list is built", &rules),
    ]
    .concat()
}

fn strategy(slug: &str, tab: Tab) -> String {
    let Some(strategy) = data::strategy(slug) else {
        return not_found();
    };

    let tabs: String = Tab::ALL
        .into_iter()
        .map(|each| {
            let route = Route::Strategy {
                slug: slug.to_string(),
                tab: each,
            };
            let path = route.path().unwrap_or_default();
            let current = if each == tab {
                " aria-current=\"page\""
            } else {
                ""
            };
            format!(
                "<a class=\"tab\" href=\"{}\"{current}>{}</a>",
                esc(&path),
                esc(each.short_label())
            )
        })
        .collect();

    let header = format!(
        "{}{}{}",
        heading(1, &strategy.name()),
        para(&format!(
            "{} · by {} · {} · {}",
            strategy.title,
            strategy.author,
            strategy.bundle.source.label(),
            strategy.standing.label()
        )),
        el("nav", "class=\"tabs\" aria-label=\"Strategy\"", &tabs)
    );

    let content = match tab {
        Tab::Overview => strategy_overview(strategy),
        Tab::Record => strategy_record(strategy),
        Tab::Code => strategy_code(strategy),
        Tab::Review => strategy_review(strategy),
        Tab::Verify => strategy_verify(strategy),
        Tab::Health => strategy_health(strategy),
    };

    format!("{header}{content}")
}

fn strategy_overview(strategy: &shared::Strategy) -> String {
    let performance = performance(strategy);
    let mut out = vec![section("What it does", &para(&strategy.description))];

    if let Some(kill) = &strategy.kill {
        out.push(section(
            "This vault was stopped",
            &format!("{}{}", para(&kill.reason), para(&kill.outcome)),
        ));
    }

    if let Some(successor) = &strategy.superseded_by {
        out.push(section(
            "A newer version exists",
            &format!(
                "{}{}",
                para(
                    "This vault runs exactly as it always has. A new version is a separate vault, \
                     which depositors choose to move to."
                ),
                para_link(&format!("/strategies/{successor}"), "See the new version")
            ),
        ));
    }

    if let Some(record) = &strategy.record {
        out.push(section(
            "The record",
            &facts(&[
                (
                    "Live".to_string(),
                    esc(&format!(
                        "{} · {} trades",
                        fmt::days(strategy.days_live),
                        fmt::count(record.trades)
                    )),
                ),
                (
                    "Normalised return".to_string(),
                    esc(&fmt::annualised(performance.normalised_annualised)),
                ),
                (
                    "Raw return".to_string(),
                    esc(&fmt::annualised(performance.raw_annualised)),
                ),
                (
                    "Max drawdown".to_string(),
                    esc(&fmt::percent_signed(performance.max_drawdown, 1)),
                ),
                (
                    "Size it was earned at".to_string(),
                    esc(&format!(
                        "an average of {}, at most {}. Returns at a much larger size are unknown.",
                        fmt::money_compact(record.average_aum),
                        fmt::money_compact(record.peak_aum)
                    )),
                ),
            ]),
        ));
    }

    if let Some(review) = &strategy.review {
        out.push(section(
            "The review",
            &format!(
                "{}{}{}",
                para(&format!(
                    "Score {} out of 100, with {} finding{}.",
                    review.score,
                    review.findings.len(),
                    if review.findings.len() == 1 { "" } else { "s" }
                )),
                para(&review.summary),
                para_link(
                    &format!("/strategies/{}/review", strategy.slug),
                    "The full review"
                )
            ),
        ));
    }

    {
        let vault = &strategy.vault;
        let venue_rows: Vec<Vec<String>> = strategy
            .legs
            .iter()
            .map(|leg| {
                vec![
                    esc(&leg.venue),
                    esc(leg.custody.label()),
                    esc(leg.exit_path.label()),
                    esc(leg.valuation.label()),
                    esc(&fmt::percent(leg.share_of_value, 0)),
                ]
            })
            .collect();

        out.push(section(
            "Where the money is, and who is stopping it leaving",
            &format!(
                "{}{}",
                table(
                    &[
                        "Venue",
                        "Custody",
                        "Exit path",
                        "Valued by",
                        "Share of the vault"
                    ],
                    &venue_rows
                ),
                para(&format!(
                    "Exit class: {}. {}",
                    vault.exit_class.label(),
                    vault.exit_class.summary()
                ))
            ),
        ));

        out.push(section(
            "The vault",
            &facts(&[
                ("Address".to_string(), esc(&vault.address)),
                ("Chain".to_string(), esc(vault.chain.label())),
                ("Value".to_string(), esc(&fmt::money(vault.value))),
                (
                    "Value per share".to_string(),
                    esc(&fmt::share_price(vault.share_price)),
                ),
                (
                    "High-water mark".to_string(),
                    esc(&fmt::share_price(vault.high_water_mark)),
                ),
                (
                    "Idle balance".to_string(),
                    esc(&format!(
                        "{} — withdrawals up to this are paid in one transaction",
                        fmt::money(vault.idle)
                    )),
                ),
                (
                    "Tank".to_string(),
                    esc(&format!(
                        "{}, about {} of charges",
                        fmt::money_exact(vault.tank.balance),
                        fmt::days(vault.tank.runway_days() as u32)
                    )),
                ),
            ]),
        ));

        let audit_rows: Vec<Vec<String>> = vault
            .audits
            .iter()
            .map(|audit| {
                vec![
                    esc(&audit.subject),
                    esc(audit.status.label()),
                    esc(&audit.note),
                ]
            })
            .collect();
        out.push(section(
            "Audit status",
            &table(&["What", "Status", "Notes"], &audit_rows),
        ));

        out.push(section(
            "Fees",
            &format!("{}{}", fee_table(), para(copy::FEE_NOTE)),
        ));
        out.push(section("Leaving", &para(copy::EXIT_NOTE)));
    }

    if !strategy.limits.is_empty() {
        out.push(section(
            "What it will not do",
            &format!(
                "{}{}",
                para(
                    "There is no platform risk policy. These limits live in the strategy's own \
                     code, where the reviewer reads them."
                ),
                list(strategy.limits.iter().map(|limit| esc(limit)))
            ),
        ));
    }

    if !strategy.dependencies.is_empty() {
        out.push(section(
            "What it depends on",
            &list(strategy.dependencies.iter().map(|item| esc(item))),
        ));
    }

    out.concat()
}

fn fee_table() -> String {
    let rows: Vec<Vec<String>> = copy::FEES
        .iter()
        .map(|(fee, on, to, level)| vec![esc(fee), esc(on), esc(to), esc(level)])
        .collect();
    table(&["Fee", "Charged on", "Paid to", "Level"], &rows)
}

fn strategy_record(strategy: &shared::Strategy) -> String {
    let Some(record) = &strategy.record else {
        return para("This strategy never traded, so it has no record.");
    };
    let performance = performance(strategy);

    let monthly: Vec<Vec<String>> = record
        .monthly
        .iter()
        .rev()
        .take(12)
        .map(|point| {
            vec![
                esc(&fmt::month_year(point.date)),
                esc(&fmt::percent_signed(point.value, 2)),
            ]
        })
        .collect();

    let trades: Vec<Vec<String>> = strategy
        .trades
        .iter()
        .take(20)
        .map(|trade| {
            vec![
                esc(&fmt::timestamp(trade.at)),
                esc(&trade.venue),
                esc(&trade.market),
                esc(trade.side.label()),
                esc(&fmt::number(trade.size, 3)),
                esc(&fmt::number(trade.price, 2)),
                esc(&trade.kind),
            ]
        })
        .collect();

    let events: Vec<Vec<String>> = strategy
        .events
        .iter()
        .take(15)
        .map(|event| {
            vec![
                esc(&fmt::timestamp(event.at)),
                esc(event.kind.label()),
                esc(&event.summary),
                event
                    .amount
                    .map_or_else(String::new, |amount| esc(&fmt::money_exact(amount))),
            ]
        })
        .collect();

    [
        section(
            "Read from the chain",
            &facts(&[
                (
                    "Value per share".to_string(),
                    esc(&fmt::share_price(
                        record.value_per_share.last().map_or(1.0, |p| p.value),
                    )),
                ),
                (
                    "Total return".to_string(),
                    esc(&fmt::percent_signed(performance.total_return, 1)),
                ),
                (
                    "Normalised".to_string(),
                    esc(&fmt::annualised(performance.normalised_annualised)),
                ),
                (
                    "Raw".to_string(),
                    esc(&fmt::annualised(performance.raw_annualised)),
                ),
                (
                    "Realised volatility".to_string(),
                    esc(&fmt::percent(performance.volatility, 1)),
                ),
                (
                    "Max drawdown".to_string(),
                    esc(&fmt::percent_signed(performance.max_drawdown, 1)),
                ),
                ("Trades".to_string(), esc(&fmt::count(record.trades))),
                (
                    "Capital".to_string(),
                    esc(&format!(
                        "{} now, {} on average, {} at most",
                        fmt::money_compact(strategy.capital()),
                        fmt::money_compact(record.average_aum),
                        fmt::money_compact(record.peak_aum)
                    )),
                ),
            ]),
        ),
        section("By month", &table(&["Month", "Return"], &monthly)),
        section(
            "Recent trades",
            &table(
                &["When", "Venue", "Market", "Side", "Size", "Price", "Kind"],
                &trades,
            ),
        ),
        section(
            "Vault events",
            &table(&["When", "Event", "What happened", "Amount"], &events),
        ),
    ]
    .concat()
}

fn strategy_code(strategy: &shared::Strategy) -> String {
    let bundle = &strategy.bundle;
    let mut out = vec![section(
        "The bundle",
        &facts(&[
            ("Bundle hash".to_string(), esc(&bundle.bundle_hash)),
            ("Component hash".to_string(), esc(&bundle.component_hash)),
            ("Source hash".to_string(), esc(&bundle.source_hash)),
            ("Manifest hash".to_string(), esc(&bundle.manifest_hash)),
            ("World".to_string(), esc(&bundle.world)),
            ("Toolchain".to_string(), esc(&bundle.toolchain)),
            ("Builder image".to_string(), esc(&bundle.builder_image)),
            (
                "Source".to_string(),
                match &bundle.source {
                    shared::Source::Published { repo, commit } => {
                        esc(&format!("published · {repo} at {commit}"))
                    }
                    shared::Source::Sealed => esc(
                        "sealed: stored encrypted and released only to attested builders, \
                         reviewers and executors. The reviewer read it.",
                    ),
                },
            ),
        ]),
    )];

    if !bundle.models.is_empty() {
        let rows: Vec<Vec<String>> = bundle
            .models
            .iter()
            .map(|model| {
                vec![
                    esc(&model.name),
                    esc(&model.opset.to_string()),
                    esc(&format!("{:.0} MiB", model.size_mib)),
                    esc(&model.purpose),
                ]
            })
            .collect();
        out.push(section(
            "Models in the bundle",
            &format!(
                "{}{}",
                para(
                    "Weights are data, not source: nobody can read them. The review judges a \
                     model by its inputs, outputs and operators, and by what the code does with \
                     its output."
                ),
                table(&["Name", "Opset", "Size", "What it is for"], &rows)
            ),
        ));
    }

    if !bundle.allowances.placement.is_empty() {
        out.push(section(
            "What it may use",
            &facts(&[
                (
                    "Fuel".to_string(),
                    esc(&format!(
                        "{} a second",
                        fmt::count(
                            u32::try_from(bundle.allowances.fuel_per_sec).unwrap_or(u32::MAX)
                        )
                    )),
                ),
                (
                    "Memory".to_string(),
                    esc(&format!("{} MiB", bundle.allowances.memory_mib)),
                ),
                (
                    "Bandwidth".to_string(),
                    esc(&format!(
                        "{} requests a minute, {} a day, {} streams",
                        fmt::count(bundle.allowances.requests_per_min),
                        fmt::bytes(
                            u32::try_from(bundle.allowances.bytes_per_day).unwrap_or(u32::MAX)
                        ),
                        bundle.allowances.streams
                    )),
                ),
                (
                    "Inference".to_string(),
                    esc(&format!(
                        "{} ms a minute",
                        bundle.allowances.inference_ms_per_min
                    )),
                ),
                ("Placement".to_string(), esc(&bundle.allowances.placement)),
            ]),
        ));
    }

    if !bundle.secrets.is_empty() {
        out.push(section(
            "Sealed secrets",
            &format!(
                "{}{}",
                para(
                    "Values sealed to this bundle's hash. Only an attested executor running \
                     exactly this bundle can read them, and they never appear in a record."
                ),
                list(bundle.secrets.iter().map(|name| esc(name)))
            ),
        ));
    }

    if let Some(manifest) = bundle
        .source_files
        .iter()
        .find(|file| file.path == "shonin.toml")
    {
        out.push(section("The manifest", &code_block(&manifest.body)));
    }

    if bundle.source_files.is_empty() {
        out.push(section(
            "The source",
            &para(
                "This strategy's source is sealed, so the project isn't served here. The \
                 attested builder compiled it and the reviewer read it; what sealing changes is \
                 who else can.",
            ),
        ));
    } else {
        let files: Vec<Vec<String>> = bundle
            .source_files
            .iter()
            .map(|file| vec![esc(&file.path), esc(&fmt::bytes(file.bytes))])
            .collect();
        out.push(section("The project", &table(&["File", "Size"], &files)));
        if let Some(main) = bundle
            .source_files
            .iter()
            .find(|file| file.path == "src/lib.rs")
        {
            out.push(section("src/lib.rs", &code_block(&main.body)));
        }
    }

    let log: String = bundle
        .build_log
        .iter()
        .map(|line| line.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    out.push(section("The build log", &code_block(&log)));

    out.concat()
}

fn strategy_review(strategy: &shared::Strategy) -> String {
    let Some(review) = &strategy.review else {
        return section(
            "Review pending",
            &para(
                "The reviewer hasn't scored this strategy yet. The score appears here when it is signed.",
            ),
        );
    };

    let findings: String = if review.findings.is_empty() {
        para("No findings.")
    } else {
        review
            .findings
            .iter()
            .map(|finding| {
                el(
                    "article",
                    "class=\"card stack-sm\"",
                    &format!(
                        "{}{}{}{}",
                        heading(3, &finding.summary),
                        para(&format!(
                            "{} · {} · {}:{}",
                            finding.category.label(),
                            finding.severity.label(),
                            finding.file,
                            finding.line
                        )),
                        para(&finding.detail),
                        String::new()
                    ),
                )
            })
            .collect()
    };

    [
        section(
            "The score",
            &format!(
                "{}{}{}",
                para(&format!("{} out of 100.", review.score)),
                para(&review.summary),
                para(copy::REVIEW_NOTE)
            ),
        ),
        section("Findings", &findings),
        section(
            "What produced it",
            &facts(&[
                ("Reviewed".to_string(), esc(&fmt::date(review.reviewed))),
                ("Reviewer image".to_string(), esc(&review.reviewer_image)),
                ("Model".to_string(), esc(&review.model_id_hash)),
                ("Prompt and schema".to_string(), esc(&review.prompt_hash)),
                ("Full report".to_string(), esc(&review.report_hash)),
                ("Paid for by".to_string(), esc(&review.bought_by)),
            ]),
        ),
        section(
            "What a review can't tell you",
            &list(review.limits.iter().map(|limit| esc(limit))),
        ),
    ]
    .concat()
}

fn strategy_verify(strategy: &shared::Strategy) -> String {
    let Some(attestation) = &strategy.attestation else {
        return para("Nothing is running for this strategy, so no keys are blessed for it.");
    };

    let keys: Vec<Vec<String>> = attestation
        .keys
        .iter()
        .map(|key| {
            vec![
                esc(key.role.label()),
                esc(&key.address),
                esc(&key.zone),
                esc(&fmt::timestamp(key.blessed_until)),
                esc(&key.proof_tx),
            ]
        })
        .collect();

    let approvals: Vec<Vec<String>> = attestation
        .approvals
        .iter()
        .map(|approval| {
            vec![
                esc(&approval.venue),
                esc(&approval.detail),
                esc(approval.custody.label()),
            ]
        })
        .collect();

    let checks: Vec<Vec<String>> = copy::VERIFY_CHECKS
        .iter()
        .map(|(claim, how, needs)| vec![esc(claim), esc(how), esc(needs)])
        .collect();

    [
        heading(2, "Which code is trading this vault?"),
        facts(&[
            ("Strategy".to_string(), esc(&strategy.name())),
            ("Bundle".to_string(), esc(&strategy.bundle.bundle_hash)),
            (
                "Built from".to_string(),
                esc(&format!(
                    "{} by builder {}",
                    strategy.bundle.source_hash, attestation.builder.digest
                )),
            ),
            (
                "Executor image".to_string(),
                esc(&format!(
                    "{} · allowlisted {} · audit {}",
                    attestation.image.digest,
                    fmt::date(attestation.image.allowlisted),
                    attestation.image.audit.label()
                )),
            ),
            ("Trust root".to_string(), esc(&attestation.trust_root)),
            (
                "Blessing lasts".to_string(),
                esc(&format!(
                    "{} hours, renewed before it expires",
                    attestation.blessing_ttl_hours
                )),
            ),
        ]),
        section(
            "Blessed keys",
            &table(&["Role", "Key", "Zone", "Blessed until", "Proof"], &keys),
        ),
        section(
            "What the venues have approved",
            &table(&["Venue", "Approval", "Custody"], &approvals),
        ),
        section(
            "What anyone can check",
            &format!(
                "{}{}",
                table(&["Claim", "How to check it", "What you need"], &checks),
                para(copy::VERIFY_LIMIT)
            ),
        ),
    ]
    .concat()
}

fn strategy_health(strategy: &shared::Strategy) -> String {
    let Some(health) = &strategy.health else {
        return para("Nothing is running for this strategy, so there is no health report.");
    };

    let signals: Vec<Vec<String>> = health
        .signals
        .iter()
        .map(|signal| {
            vec![
                esc(&signal.name),
                esc(&format!("{} {}", fmt::number(signal.value, 1), signal.unit)),
                esc(&fmt::number(signal.warn_at, 1)),
                esc(&fmt::number(signal.error_at, 1)),
                esc(signal.level().label()),
            ]
        })
        .collect();

    let entries: Vec<Vec<String>> = health
        .entries
        .iter()
        .map(|entry| {
            vec![
                esc(&fmt::timestamp(entry.at)),
                esc(entry.level.label()),
                esc(&entry.message),
            ]
        })
        .collect();

    [
        section(
            "How it is running",
            &format!(
                "{}{}",
                para(&health.summary()),
                para(
                    "A health report is signed by the executor and says how a strategy is \
                     running. It is operational evidence, not the record: what the strategy did \
                     is on the chain."
                )
            ),
        ),
        section(
            "Signals",
            &table(
                &["Signal", "Now", "Warning at", "Error at", "State"],
                &signals,
            ),
        ),
        section(
            "Recent entries",
            &table(&["When", "Level", "What"], &entries),
        ),
    ]
    .concat()
}

fn author(handle: &str) -> String {
    let Some(author) = data::author(handle) else {
        return not_found();
    };
    let published = data::by_author(handle);

    let rows: Vec<Vec<String>> = published
        .iter()
        .map(|strategy| {
            vec![
                link(&format!("/strategies/{}", strategy.slug), &strategy.name()),
                esc(strategy.standing.label()),
                esc(&fmt::days(strategy.days_live)),
                strategy
                    .review
                    .as_ref()
                    .map_or_else(|| "—".to_string(), |review| review.score.to_string()),
                esc(&fmt::money_compact(strategy.capital())),
            ]
        })
        .collect();

    [
        heading(1, handle),
        para(&author.blurb),
        para(&format!("Publishing since {}.", fmt::date(author.joined))),
        section(
            "Everything they have published",
            &format!(
                "{}{}",
                para(
                    "Every strategy an author has registered is here: live, new, superseded, \
                     retired, and the builds that failed. Publishing leaves a permanent record."
                ),
                table(
                    &["Strategy", "Standing", "Live", "Review", "Capital"],
                    &rows
                )
            ),
        ),
    ]
    .concat()
}

fn portfolio() -> String {
    [
        heading(1, "Your positions"),
        para(
            "What you hold across vaults, with your pending claims and how correlated your \
             holdings are with each other.",
        ),
        para("This page is built in your browser from your own wallet, so there is nothing to show until you connect one."),
        para_link("/strategies", "Browse the strategies"),
    ]
    .concat()
}

fn queue() -> String {
    let rows: Vec<Vec<String>> = data::get()
        .jobs
        .iter()
        .map(|job| {
            vec![
                link(&format!("/queue/{}", job.id), &job.id),
                esc(&job.subject),
                esc(&job.author),
                esc(job.state.label()),
                esc(&fmt::ago(job.submitted, data::now())),
            ]
        })
        .collect();

    [
        heading(1, "Build queue"),
        para(
            "First come, first served, one job in flight per author. Every job shows its state \
             and its full log, whether it succeeded or failed — an author can't quietly try \
             twenty times and show you the twentieth.",
        ),
        table(&["Build", "Subject", "Author", "State", "Submitted"], &rows),
    ]
    .concat()
}

fn job(id: &str) -> String {
    let Some(job) = data::job(id) else {
        return not_found();
    };

    let log: String = job
        .log
        .iter()
        .map(|line| line.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");

    let mut facts_rows = vec![
        ("Subject".to_string(), esc(&job.subject)),
        ("Author".to_string(), esc(&job.author)),
        ("State".to_string(), esc(job.state.label())),
        ("Submitted".to_string(), esc(&fmt::timestamp(job.submitted))),
        ("Source hash".to_string(), esc(&job.source_hash)),
    ];
    if let Some(bundle) = &job.bundle_hash {
        facts_rows.push(("Bundle hash".to_string(), esc(bundle)));
    }
    if job.duration_seconds > 0 {
        facts_rows.push((
            "Took".to_string(),
            esc(&fmt::duration(job.duration_seconds)),
        ));
    }

    let mut out = vec![heading(1, &format!("Build {}", job.id)), facts(&facts_rows)];

    if let shared::JobState::Failed { reason } = &job.state {
        out.push(section(
            "It failed",
            &format!(
                "{}{}",
                para(reason),
                para(
                    "The build is charged, because it ran. The vault closes and the author \
                     reclaims the rest of its tank. A fixed resubmission is a new vault."
                )
            ),
        ));
    }

    if let Some(slug) = &job.strategy_slug {
        out.push(para_link(
            &format!("/strategies/{slug}"),
            "The strategy this build belongs to",
        ));
    }

    out.push(section(
        "The log",
        &if log.is_empty() {
            para("Nothing yet: this job is still waiting for a builder.")
        } else {
            code_block(&log)
        },
    ));

    out.concat()
}

fn publish() -> String {
    let steps = list(
        copy::PUBLISH_STEPS
            .iter()
            .map(|(title, body)| format!("{} {}", el("strong", "", &esc(title)), esc(body))),
    );

    [
        heading(1, "Publish a strategy"),
        para(
            "You submit a Rust project; we compile it. The bundle a vault runs is an artifact of \
             our build, not a file someone handed us, so every strategy on the platform has \
             source behind it that the reviewer read.",
        ),
        section("How it goes", &steps),
        section(
            "What it costs",
            &format!(
                "{}{}",
                para(
                    "There is no publishing fee. Creating the vault funds its tank, and the \
                     build and the review are its first charges: the metered build, a flat $1 \
                     review, and enough for a week at the allowances the strategy declares."
                ),
                para(copy::TANK_NOTE)
            ),
        ),
        section(
            "Published or sealed",
            &para(
                "Source is public by default. An author who needs to protect an edge can seal it \
                 instead: the build, the hashes and the review are identical either way, and what \
                 changes is who else can read the source.",
            ),
        ),
        para_link("/queue", "Watch the build queue"),
    ]
    .concat()
}

fn status() -> String {
    let items: String = data::get()
        .feed
        .iter()
        .map(|event| {
            let link_html = event
                .link
                .as_ref()
                .map_or_else(String::new, |href| para_link(href, "More"));
            el(
                "article",
                "class=\"card stack-sm\"",
                &format!(
                    "{}{}{}{}",
                    heading(3, &event.title),
                    para(&format!(
                        "{} · {}",
                        event.kind.label(),
                        fmt::timestamp(event.at)
                    )),
                    para(&event.detail),
                    link_html
                ),
            )
        })
        .collect();

    [
        heading(1, "Status"),
        para(
            "Every kill with its reason, every change to the image allowlist, and every venue \
             that stopped behaving. Nothing here is a promise that we'll never halt a vault — it \
             is a record of when we have.",
        ),
        items,
    ]
    .concat()
}

fn verify() -> String {
    let checks: Vec<Vec<String>> = copy::VERIFY_CHECKS
        .iter()
        .map(|(claim, how, needs)| vec![esc(claim), esc(how), esc(needs)])
        .collect();

    let audits: Vec<Vec<String>> = data::vaults()
        .first()
        .map(|strategy| {
            strategy
                .vault
                .audits
                .iter()
                .map(|audit| {
                    vec![
                        esc(&audit.subject),
                        esc(audit.status.label()),
                        esc(&audit.note),
                    ]
                })
                .collect()
        })
        .unwrap_or_default();

    [
        heading(1, "Verify"),
        para(
            "Nothing here asks to be taken on trust. Everything the platform claims about a vault \
             is something you can check yourself, with a chain RPC and the published source.",
        ),
        section(
            "What anyone can check",
            &table(&["Claim", "How to check it", "What you need"], &checks),
        ),
        section("The limit of it", &para(copy::VERIFY_LIMIT)),
        section(
            "Audit status",
            &format!(
                "{}{}",
                para(
                    "Vaults open to deposits before every audit is finished, and every vault page \
                     shows where each one stands, so you can size your own exposure."
                ),
                table(&["What", "Status", "Notes"], &audits)
            ),
        ),
        section(
            "Changing the code that runs vaults",
            &para(
                "A strategy never changes: it is what the depositor chose. The executor is our \
                 code and has to be patchable, so a new image is proposed publicly and can only \
                 be blessed after a two-day timelock — long enough for anyone to rebuild it from \
                 source and object. Removing an image is instant. A running vault adopts a new \
                 image by failover, with its own on-chain approval as the handoff.",
            ),
        ),
    ]
    .concat()
}

fn about() -> String {
    let guarantees: Vec<Vec<String>> = copy::GUARANTEES
        .iter()
        .map(|(title, body, rules_out)| vec![esc(title), esc(body), esc(rules_out)])
        .collect();

    let not_this = list(
        copy::NOT_THIS
            .iter()
            .map(|(title, body)| format!("{} {}", el("strong", "", &esc(title)), esc(body))),
    );

    let terms: Vec<Vec<String>> = copy::TERMS
        .iter()
        .map(|(term, definition)| vec![esc(term), esc(definition)])
        .collect();

    [
        heading(1, "About Shonin"),
        para(copy::PITCH),
        para(copy::HOME_LEDE),
        section("The trust bar", &para(copy::HALT_NEVER_STEAL)),
        section(
            "What you are promised",
            &table(
                &["Guarantee", "What it means", "What it rules out"],
                &guarantees,
            ),
        ),
        section(
            "Fees",
            &format!(
                "{}{}{}",
                fee_table(),
                para(copy::FEE_NOTE),
                para(copy::TANK_NOTE)
            ),
        ),
        section("Leaving a vault", &para(copy::EXIT_NOTE)),
        section("The review", &para(copy::REVIEW_NOTE)),
        section("What Shonin is not", &not_this),
        section(
            "Words used here",
            &table(&["Term", "What it means"], &terms),
        ),
    ]
    .concat()
}

fn not_found() -> String {
    [
        heading(1, "Page not found"),
        para("There's no page at this address."),
        list(vec![
            link("/", "Home"),
            link("/strategies", "Strategies"),
            link("/verify", "Verify"),
        ]),
    ]
    .concat()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every route must pre-render something substantial: an empty page would
    /// be a page crawlers and readers without JavaScript see as blank.
    #[test]
    fn every_route_renders_a_real_page() {
        for route in Route::all_pages() {
            let html = body(&route);
            assert!(
                html.len() > 1000,
                "{:?} pre-renders only {} bytes",
                route.path(),
                html.len()
            );
            assert!(html.contains("<h1>"), "{:?} has no heading", route.path());
            assert!(
                html.contains("masthead"),
                "{:?} has no header",
                route.path()
            );
        }
    }

    /// The pre-rendered view is a plainer view of the same data, so the facts
    /// a strategy's page turns on have to appear in it. If a page grows a
    /// figure the pre-render doesn't carry, a reader without JavaScript — and
    /// every link-preview bot — sees a different page from everyone else.
    #[test]
    fn a_strategys_pre_render_carries_the_same_facts() {
        for strategy in data::vaults() {
            let html = body(&Route::Strategy {
                slug: strategy.slug.clone(),
                tab: Tab::Overview,
            });

            assert!(
                html.contains(&esc(&strategy.name())),
                "{}: no name",
                strategy.slug
            );
            assert!(
                html.contains(&esc(&strategy.author)),
                "{}: no author",
                strategy.slug
            );
            assert!(
                html.contains(&esc(&strategy.description)),
                "{}: no description",
                strategy.slug
            );

            if let Some(review) = &strategy.review {
                assert!(
                    html.contains(&format!("Score {} out of 100", review.score)),
                    "{}: no review score",
                    strategy.slug
                );
            }
            let vault = &strategy.vault;
            assert!(
                html.contains(&esc(&fmt::money(vault.value))),
                "{}: no value",
                strategy.slug
            );
            assert!(
                html.contains(&esc(vault.exit_class.label())),
                "{}: no exit class",
                strategy.slug
            );
            for leg in &strategy.legs {
                assert!(
                    html.contains(&esc(&leg.venue)),
                    "{}: no {} leg",
                    strategy.slug,
                    leg.venue
                );
            }
        }
    }

    /// Whatever the route, the pre-render and the app read the same data, so a
    /// figure shown on one is the figure shown on the other.
    #[test]
    fn the_home_page_carries_the_platform_totals() {
        let html = body(&Route::Home);
        assert!(html.contains(&esc(copy::PITCH)));
        assert!(html.contains(&esc(&fmt::money_compact(data::total_vault_capital()))));
        assert!(html.contains(&esc(&fmt::count(data::total_trades()))));
        for strategy in data::ranked().into_iter().take(5) {
            assert!(
                html.contains(&esc(&strategy.name())),
                "the home page doesn't list {}",
                strategy.slug
            );
        }
    }

    /// A page a reader can't leave is a dead end, so every pre-rendered page
    /// carries the navigation and the footer.
    #[test]
    fn every_page_can_be_left_again() {
        for route in Route::indexable() {
            let html = body(&route);
            assert!(html.contains("class=\"mainnav\""), "{:?}", route.path());
            assert!(html.contains("colophon"), "{:?}", route.path());
            assert!(html.contains("skip-link"), "{:?}", route.path());
        }
    }

    #[test]
    fn the_not_found_page_renders_too() {
        let html = body(&Route::NotFound);
        assert!(html.contains("Page not found"));
    }

    /// Nothing in the data escapes into the markup as HTML.
    #[test]
    fn text_from_the_data_is_escaped() {
        assert_eq!(
            esc("<script>a & b</script>"),
            "&lt;script&gt;a &amp; b&lt;/script&gt;"
        );
        let html = body(&Route::Strategy {
            slug: data::vaults()[0].slug.clone(),
            tab: Tab::Code,
        });
        // Source listings are full of angle brackets; none of them are tags.
        assert!(!html.contains("<Vec<"));
    }
}
