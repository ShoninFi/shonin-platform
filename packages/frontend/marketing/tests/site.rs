//! What can break without anyone noticing: a page not written, a link to
//! nothing, a placeholder shipped, wallet or API code creeping into a site
//! that must have none, or an inline script or style the
//! Content-Security-Policy would block.

use std::collections::{BTreeMap, BTreeSet};

use marketing::{Links, site};

fn links() -> Links {
    Links {
        app: "https://app.shonin.fi".into(),
        docs: "https://docs.shonin.fi".into(),
        site: "https://shonin.fi".into(),
    }
}

fn files() -> BTreeMap<String, Vec<u8>> {
    site(&links())
        .expect("the site builds")
        .into_iter()
        .map(|file| (file.path, file.bytes))
        .collect()
}

fn pages(files: &BTreeMap<String, Vec<u8>>) -> BTreeMap<String, String> {
    files
        .iter()
        .filter(|(path, _)| path.ends_with(".html"))
        .map(|(path, bytes)| (path.clone(), String::from_utf8(bytes.clone()).unwrap()))
        .collect()
}

/// Every value of `attr="..."` in `html`.
fn attributes<'a>(html: &'a str, attr: &str) -> Vec<&'a str> {
    let needle = format!(" {attr}=\"");
    html.match_indices(&needle)
        .map(|(at, _)| {
            let rest = &html[at + needle.len()..];
            &rest[..rest.find('"').unwrap()]
        })
        .collect()
}

/// The file a same-origin path is served from, as Workers static assets
/// serve it.
fn served(path: &str) -> String {
    let path = path.trim_start_matches('/');
    if path.is_empty() {
        "index.html".into()
    } else if path.contains('.') {
        path.into()
    } else {
        format!("{path}.html")
    }
}

#[test]
fn every_page_and_file_the_worker_serves_is_written() {
    let files = files();
    for path in [
        "index.html",
        "authors.html",
        "terms.html",
        "privacy.html",
        "404.html",
        "_headers",
        "_redirects",
        "robots.txt",
    ] {
        assert!(files.contains_key(path), "{path} is not written");
    }
    assert!(
        files
            .keys()
            .any(|p| p.starts_with("assets/site.") && p.ends_with(".css"))
    );
    assert!(
        files
            .keys()
            .any(|p| p.starts_with("assets/site.") && p.ends_with(".js"))
    );
    assert_eq!(
        files.keys().filter(|p| p.ends_with(".woff2")).count(),
        6,
        "the six self-hosted faces"
    );
}

#[test]
fn every_internal_link_and_asset_resolves_to_a_written_file_and_anchor() {
    let files = files();
    let pages = pages(&files);
    let ids: BTreeMap<&String, BTreeSet<&str>> = pages
        .iter()
        .map(|(path, html)| (path, attributes(html, "id").into_iter().collect()))
        .collect();
    for (page, html) in &pages {
        let mut targets = attributes(html, "href");
        targets.extend(attributes(html, "src"));
        for target in targets {
            if target.starts_with("https://") || target.starts_with("url(") {
                continue;
            }
            assert!(
                !target.starts_with("http:"),
                "{page} links to {target} over plain http"
            );
            let (path, fragment) = target.split_once('#').unwrap_or((target, ""));
            let file = if path.is_empty() {
                page.clone()
            } else {
                assert!(path.starts_with('/'), "{page}: {target} is not rooted");
                served(path)
            };
            assert!(
                files.contains_key(&file),
                "{page} links to {target}, but {file} is not written"
            );
            if !fragment.is_empty() && file.ends_with(".html") {
                assert!(
                    ids[&file].contains(fragment),
                    "{page} links to {target}, but {file} has no #{fragment}"
                );
            }
        }
    }
}

#[test]
fn app_links_go_to_the_app_origin_in_full() {
    let pages = pages(&files());
    for (page, html) in &pages {
        for href in attributes(html, "href") {
            for route in ["/agents", "/portfolio", "/publish", "/status"] {
                if href.starts_with(route) {
                    panic!("{page} links to {href} on this origin, not the app's");
                }
            }
        }
        assert!(html.contains("https://app.shonin.fi/agents"), "{page}");
    }
}

#[test]
fn every_docs_link_names_a_page_the_docs_publish() {
    let content = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../docs/content");
    let pages = pages(&files());
    let mut seen = 0;
    for (page, html) in &pages {
        for href in attributes(html, "href") {
            let Some(path) = href.strip_prefix("https://docs.shonin.fi/") else {
                continue;
            };
            seen += 1;
            let path = path.split('#').next().unwrap().trim_end_matches('/');
            let source = if path.is_empty() {
                content.join("index.md")
            } else {
                content.join(format!("{path}.md"))
            };
            assert!(
                source.is_file() || content.join(path).join("index.md").is_file(),
                "{page} links to {href}, which the docs don't publish"
            );
        }
    }
    assert!(seen > 0, "no page links the docs");
}

#[test]
fn no_page_ships_a_bracketed_placeholder_or_an_unfilled_template() {
    for (page, html) in pages(&files()) {
        assert!(!html.contains("{{"), "{page} has an unfilled placeholder");
        let bytes = html.as_bytes();
        for (at, _) in html.match_indices('[') {
            let next = &bytes[at + 1..(at + 3).min(bytes.len())];
            assert!(
                !(next.len() == 2 && next.iter().all(u8::is_ascii_uppercase)),
                "{page} has a bracketed placeholder near {:?}",
                &html[at..(at + 40).min(html.len())]
            );
        }
    }
}

#[test]
fn the_dist_carries_no_wallet_or_api_code() {
    let files = files();
    // The one place the lowercase name may appear is the address of Solana's
    // mark. Anything else (`window.solana`, `@solana/`, a `solana:` URI) is
    // wallet code.
    let mark = files
        .keys()
        .find(|path| path.starts_with("assets/chains/solana."))
        .map(|path| format!("/{path}"))
        .expect("Solana's mark");
    for (path, bytes) in files {
        let text = String::from_utf8_lossy(&bytes).replace(&mark, "");
        for word in [
            "ethereum",
            "solana",
            "phantom",
            "wallet_",
            "testnet-api",
            "fetch(",
            "XMLHttpRequest",
            "localStorage",
            "document.cookie",
        ] {
            assert!(!text.contains(word), "{path} contains {word}");
        }
        assert!(!text.contains("Connect wallet"), "{path} offers a wallet");
    }
}

#[test]
fn pages_run_only_the_hashed_boot_script_and_carry_no_inline_styles() {
    let files = files();
    let headers = String::from_utf8(files["_headers"].clone()).unwrap();
    assert!(headers.contains(&marketing::boot_hash()));
    assert!(headers.contains("img-src 'self';"));
    assert!(headers.contains("frame-ancestors 'none'"));
    assert!(!headers.contains("unsafe-inline"));
    for (page, html) in pages(&files) {
        assert!(!html.contains(" style=\""), "{page} has an inline style");
        assert!(!html.contains("<style"), "{page} has a style element");
        assert_eq!(
            html.matches("<script").count(),
            2,
            "{page}: the boot script and the site script"
        );
        for handler in [" onclick=", " onload=", " onerror=", "javascript:"] {
            assert!(!html.contains(handler), "{page} has an inline handler");
        }
        assert_eq!(html.matches("<h1").count(), 1, "{page} has one h1");
        assert!(
            html.contains("<main id=\"content\""),
            "{page} has the skip link's target"
        );
        assert!(html.contains("noindex"), "{page} keeps noindex");
    }
}

#[test]
fn the_stylesheet_takes_its_colours_from_tokens() {
    let css = include_str!("../assets/site.css");
    let end_of_tokens = css.find("/* -----").expect("the token block ends");
    let rules = &css[end_of_tokens..];
    for (at, _) in rules.match_indices('#') {
        let hex: String = rules[at + 1..]
            .chars()
            .take_while(char::is_ascii_hexdigit)
            .collect();
        assert!(
            !(hex.len() == 3 || hex.len() == 6),
            "a colour literal outside the tokens: #{hex}"
        );
    }
    assert!(
        !rules.contains("rgb("),
        "an rgb() literal outside the tokens"
    );
    assert!(
        !rules.contains("rgba("),
        "an rgba() literal outside the tokens"
    );
}

#[test]
fn the_terms_quote_the_statement_the_vault_contract_checks() {
    let upkeep = include_str!("../../../../contracts/evm/src/vault/Upkeep.sol");
    assert!(
        upkeep.contains(&format!("\"{}\"", marketing::pages::legal::STATEMENT)),
        "the vault's statement changed; the terms must say it word for word"
    );
    let terms = String::from_utf8(files()["terms.html"].clone()).unwrap();
    assert!(terms.contains(marketing::pages::legal::STATEMENT));
}

/// The four chains, their marks' file stems, and the name each file carries.
const CHAINS: [(&str, &str); 4] = [
    ("hyperliquid", "Hyperliquid"),
    ("solana", "Solana"),
    ("base", "Base"),
    ("robinhood-chain", "Robinhood Chain"),
];

#[test]
fn every_chain_mark_is_written_named_and_shown() {
    let files = files();
    let home = String::from_utf8(files["index.html"].clone()).unwrap();
    let sources = include_str!("../assets/chains/SOURCES.md");
    let on_disk: BTreeSet<String> =
        std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/chains"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .filter(|name| name.ends_with(".svg"))
            .collect();
    assert_eq!(
        on_disk.len(),
        CHAINS.len(),
        "one mark per chain: {on_disk:?}"
    );
    for (stem, name) in CHAINS {
        assert!(on_disk.contains(&format!("{stem}.svg")), "{stem}.svg");
        let (path, bytes) = files
            .iter()
            .find(|(path, _)| {
                path.starts_with(&format!("assets/chains/{stem}.")) && path.ends_with(".svg")
            })
            .unwrap_or_else(|| panic!("the {name} mark is not written"));
        let svg = String::from_utf8(bytes.clone()).unwrap();
        assert!(svg.starts_with("<svg "), "{path}: no prolog or comments");
        assert!(
            svg.contains(&format!(
                "role=\"img\" aria-label=\"{name}\"><title>{name}</title>"
            )),
            "{path} is named {name}"
        );
        for junk in [
            "<?xml",
            "<!--",
            "<style",
            "data-name",
            "Layer_1",
            "<script",
            "href=",
        ] {
            assert!(!svg.contains(junk), "{path} carries {junk}");
        }
        assert!(
            home.contains(&format!("/{path}\"")),
            "the home page shows the {name} mark"
        );

        assert!(
            sources.contains(&format!("`{stem}.svg`")),
            "SOURCES.md records where {stem}.svg came from"
        );
    }
    assert!(
        !home.contains("art-initial"),
        "no initial tiles left in the hero"
    );
}

#[test]
fn the_privacy_page_names_cloudflare_web_analytics() {
    let files = files();
    let privacy = String::from_utf8(files["privacy.html"].clone()).unwrap();
    assert!(privacy.contains("Cloudflare Web Analytics"));
    assert!(privacy.contains("cookieless"));
    for (page, html) in pages(&files) {
        let lower = html.to_lowercase();
        for claim in ["no analytics", "no tracking"] {
            assert!(!lower.contains(claim), "{page} still says {claim:?}");
        }
    }
}

#[test]
fn the_policy_allows_only_this_origin_and_cloudflare_web_analytics() {
    let headers = String::from_utf8(files()["_headers"].clone()).unwrap();
    let csp = headers
        .lines()
        .find_map(|line| line.trim().strip_prefix("Content-Security-Policy: "))
        .expect("a Content-Security-Policy");
    let mut hosts = BTreeSet::new();
    for directive in csp.split("; ") {
        let mut parts = directive.split(' ');
        let name = parts.next().unwrap();
        let values: Vec<&str> = parts.collect();
        match name {
            "script-src" => assert_eq!(
                values,
                [
                    "'self'",
                    marketing::boot_hash().as_str(),
                    marketing::ANALYTICS_SCRIPT
                ]
            ),
            "connect-src" => assert_eq!(values, [marketing::ANALYTICS_REPORT]),
            _ => {}
        }
        for value in values {
            if !value.starts_with('\'') {
                hosts.insert(value);
            }
        }
    }
    assert_eq!(
        hosts,
        BTreeSet::from([marketing::ANALYTICS_SCRIPT, marketing::ANALYTICS_REPORT]),
        "nothing outside 'self' but the analytics script's host and its endpoint"
    );
    assert_eq!(
        marketing::ANALYTICS_SCRIPT,
        "https://static.cloudflareinsights.com"
    );
    assert_eq!(
        marketing::ANALYTICS_REPORT,
        "https://cloudflareinsights.com/cdn-cgi/rum"
    );
}

/// Where `_redirects` sends `path`, read as Workers read it: the first rule
/// that matches, `*` as a greedy splat, `:name` as one non-empty segment.
fn redirect(rules: &str, path: &str) -> Option<(String, u16)> {
    for line in rules.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        let [from, to, status] = fields[..] else {
            panic!("a rule with three fields: {line:?}");
        };
        let from: Vec<&str> = from.split('/').collect();
        let path: Vec<&str> = path.split('/').collect();
        let mut bound: Vec<(String, String)> = Vec::new();
        let mut matched = true;
        for (index, part) in from.iter().enumerate() {
            if *part == "*" {
                bound.push((":splat".into(), path.get(index..).unwrap_or(&[]).join("/")));
                break;
            }
            match path.get(index) {
                Some(segment) if part.starts_with(':') && !segment.is_empty() => {
                    bound.push(((*part).to_string(), (*segment).to_string()));
                }
                Some(segment) if segment == part => {}
                _ => {
                    matched = false;
                    break;
                }
            }
            if index + 1 == from.len() && path.len() != from.len() {
                matched = false;
            }
        }
        if matched {
            let mut to = to.to_string();
            for (name, value) in bound {
                to = to.replace(&name, &value);
            }
            return Some((to, status.parse().unwrap()));
        }
    }
    None
}

#[test]
fn the_old_app_addresses_go_to_the_app_and_nothing_else_moves() {
    let files = files();
    let rules = String::from_utf8(files["_redirects"].clone()).unwrap();
    let app = "https://app.shonin.fi";
    for (from, to) in [
        ("/agents", format!("{app}/agents")),
        (
            "/agents/basis-harvester",
            format!("{app}/agents/basis-harvester"),
        ),
        (
            "/agents/basis-harvester/review",
            format!("{app}/agents/basis-harvester/review"),
        ),
        (
            "/agents/basis-harvester/code",
            format!("{app}/agents/basis-harvester/verify"),
        ),
        (
            "/agents/basis-harvester/record",
            format!("{app}/agents/basis-harvester"),
        ),
        (
            "/agents/basis-harvester/health",
            format!("{app}/agents/basis-harvester"),
        ),
        ("/strategies", format!("{app}/agents")),
        (
            "/strategies/basis-harvester",
            format!("{app}/agents/basis-harvester"),
        ),
        (
            "/strategies/basis-harvester/code",
            format!("{app}/agents/basis-harvester"),
        ),
        ("/portfolio", format!("{app}/portfolio")),
        ("/portfolio/claims/7", format!("{app}/portfolio/claims/7")),
        ("/publish", format!("{app}/publish")),
        ("/publish/jobs/12", format!("{app}/publish/jobs/12")),
        ("/you/agents/3", format!("{app}/you/agents/3")),
        ("/status", format!("{app}/status")),
        ("/queue", format!("{app}/publish")),
        ("/queue/12", format!("{app}/publish/jobs/12")),
        ("/verify", format!("{app}/status")),
        ("/about", "/".to_string()),
        ("/authors/someone", format!("{app}/agents")),
        ("/authors/someone/agents", format!("{app}/agents")),
    ] {
        assert_eq!(redirect(&rules, from), Some((to, 301)), "{from}");
    }
    // This site's own pages and files, and anything unknown (the 404).
    for path in [
        "/",
        "/authors",
        "/terms",
        "/privacy",
        "/robots.txt",
        "/nope",
        "/agentsx",
    ] {
        assert_eq!(redirect(&rules, path), None, "{path} is not redirected");
    }
    for path in files.keys().filter(|path| !path.starts_with('_')) {
        assert_eq!(redirect(&rules, &format!("/{path}")), None, "/{path}");
    }
    // Workers want the single addresses before the patterns.
    let sources: Vec<bool> = rules
        .lines()
        .map(|line| line.split_whitespace().next().unwrap().contains([':', '*']))
        .collect();
    let first_pattern = sources.iter().position(|pattern| *pattern).unwrap();
    assert!(
        sources[first_pattern..].iter().all(|pattern| *pattern),
        "single addresses come before patterns"
    );
}
