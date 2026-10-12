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
    for (path, bytes) in files() {
        let text = String::from_utf8_lossy(&bytes);
        for word in [
            "ethereum",
            "solana",
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
