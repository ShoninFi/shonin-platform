//! The shutdown notice app.shonin.fi serves: every file it needs and no
//! other, a policy that lets nothing run but Cloudflare Web Analytics, no
//! script of its own, every link pointing at something that stays up, and
//! the address to write to, as text.

use std::collections::{BTreeMap, BTreeSet};

use marketing::{Links, shutdown};

fn links() -> Links {
    Links {
        app: "https://app.shonin.fi".into(),
        docs: "https://docs.shonin.fi".into(),
        site: "https://shonin.fi".into(),
    }
}

fn files() -> BTreeMap<String, Vec<u8>> {
    shutdown(&links())
        .expect("the notice builds")
        .into_iter()
        .map(|file| (file.path, file.bytes))
        .collect()
}

fn text(files: &BTreeMap<String, Vec<u8>>, path: &str) -> String {
    String::from_utf8(files[path].clone()).unwrap()
}

fn notice() -> String {
    text(&files(), "index.html")
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

#[test]
fn the_notice_is_written_with_its_assets_and_nothing_else() {
    let files = files();
    for path in ["index.html", "404.html", "_headers", "robots.txt"] {
        assert!(files.contains_key(path), "{path} is not written");
    }
    assert_eq!(
        files["index.html"], files["404.html"],
        "404.html is the notice"
    );
    assert!(
        files
            .keys()
            .any(|p| p.starts_with("assets/shutdown.") && p.ends_with(".css"))
    );
    assert!(
        files
            .keys()
            .any(|p| p.starts_with("share/shonin.") && p.ends_with(".png"))
    );
    assert_eq!(files.keys().filter(|p| p.ends_with(".woff2")).count(), 6);
    for path in files.keys() {
        assert!(
            !(path == "_redirects"
                || path == "sitemap.xml"
                || path.ends_with(".js")
                || path.ends_with(".wasm")),
            "{path} has no place in the notice"
        );
    }
}

#[test]
fn every_asset_and_same_origin_link_is_a_written_file() {
    let files = files();
    let html = notice();
    let mut targets = attributes(&html, "href");
    targets.extend(attributes(&html, "src"));
    for target in targets {
        if target.starts_with("https://")
            || target.starts_with("mailto:")
            || target.starts_with('#')
        {
            continue;
        }
        assert!(target.starts_with('/'), "{target} is not rooted");
        assert!(
            files.contains_key(target.trim_start_matches('/')),
            "the notice names {target}, which is not written"
        );
    }
    let css = files
        .iter()
        .find(|(p, _)| p.starts_with("assets/shutdown."))
        .map(|(_, b)| String::from_utf8(b.clone()).unwrap())
        .unwrap();
    for (at, _) in css.match_indices("url(\"/") {
        let rest = &css[at + 6..];
        let path = &rest[..rest.find('"').unwrap()];
        assert!(
            files.contains_key(path.trim_start_matches('/')),
            "the css names {path}"
        );
    }
}

#[test]
fn every_docs_link_names_a_page_the_docs_publish() {
    let content = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../docs/content");
    let html = notice();
    let mut seen = BTreeSet::new();
    for href in attributes(&html, "href") {
        let Some(path) = href.strip_prefix("https://docs.shonin.fi/") else {
            continue;
        };
        seen.insert(href);
        let path = path.split('#').next().unwrap().trim_end_matches('/');
        let source = if path.is_empty() {
            content.join("index.md")
        } else {
            content.join(format!("{path}.md"))
        };
        assert!(
            source.is_file() || content.join(path).join("index.md").is_file(),
            "the notice links to {href}, which the docs don't publish"
        );
    }
    for section in [
        "https://docs.shonin.fi/",
        "https://docs.shonin.fi/basics/trading-agents/",
        "https://docs.shonin.fi/under-the-hood/attestation/",
        "https://docs.shonin.fi/verify/",
        "https://docs.shonin.fi/writing/getting-started/",
    ] {
        assert!(seen.contains(section), "the notice doesn't link {section}");
    }
}

#[test]
fn the_notice_links_what_stays_up_and_nothing_of_the_app() {
    let html = notice();
    let hrefs = attributes(&html, "href");
    for wanted in [
        "https://shonin.fi/",
        "https://shonin.fi/terms",
        "https://shonin.fi/privacy",
        "https://github.com/ShoninFi",
        "https://github.com/ShoninFi/shonin-platform",
        "https://github.com/ShoninFi/shonin-agents",
        "https://github.com/ShoninFi/shonin-agent-template",
        "mailto:contact@shonin.fi",
    ] {
        assert!(hrefs.contains(&wanted), "the notice doesn't link {wanted}");
    }
    for href in &hrefs {
        assert!(
            !href.starts_with("https://app.shonin.fi") && !href.starts_with("http:"),
            "the notice links to {href}"
        );
    }
}

#[test]
fn the_address_is_the_mail_link_s_text_and_plain_text_too() {
    let html = notice();
    let link = "<a class=\"carry__mail\" href=\"mailto:contact@shonin.fi\">contact@shonin.fi</a>";
    assert!(html.contains(link), "the address is the link's own text");
    assert!(html.contains("<span class=\"carry__copy\">contact@shonin.fi</span>"));
    for (at, _) in html.match_indices("href=\"mailto:") {
        let rest = &html[at..];
        let text = &rest[rest.find('>').unwrap() + 1..rest.find("</a>").unwrap()];
        assert!(
            text.ends_with("contact@shonin.fi"),
            "a mail link reads {text:?}"
        );
    }
}

#[test]
fn the_notice_says_what_happened_and_that_nothing_is_held() {
    let html = notice();
    for words in [
        "Shonin is shut down",
        "8 November 2026",
        "services and test networks are retired",
        "never launched on mainnet",
        "no deposits are held",
        "nothing to withdraw",
        "Smart contracts are public and auditable. Trading agents should be too.",
    ] {
        assert!(html.contains(words), "the notice doesn't say {words:?}");
    }
}

#[test]
fn the_notice_runs_no_script_and_carries_no_inline_style() {
    let html = notice();
    assert_eq!(
        html.matches("<script").count(),
        html.matches("<script type=\"application/ld+json\">")
            .count(),
        "only structured data, which never runs"
    );
    assert!(!html.contains(" style=\""));
    assert!(!html.contains("<style"));
    for handler in [" onclick=", " onload=", " onerror=", "javascript:"] {
        assert!(!html.contains(handler));
    }
    assert_eq!(html.matches("<h1").count(), 1);
    assert!(html.contains("<main id=\"content\""));
    assert!(html.contains("<a class=\"skip\" href=\"#content\">"));
    assert!(!html.contains("{{"), "an unfilled placeholder");
}

#[test]
fn the_notice_is_kept_out_of_search() {
    let files = files();
    let html = text(&files, "index.html");
    assert!(html.contains("<meta name=\"robots\" content=\"noindex\">"));
    assert!(!html.contains("rel=\"canonical\""));
    assert_eq!(text(&files, "robots.txt"), "User-agent: *\nDisallow: /\n");
}

#[test]
fn the_notice_previews_with_the_share_image_at_its_own_address() {
    let files = files();
    let html = text(&files, "index.html");
    let image = files
        .keys()
        .find(|p| p.starts_with("share/shonin."))
        .unwrap();
    assert!(html.contains(&format!(
        "<meta property=\"og:image\" content=\"https://app.shonin.fi/{image}\">"
    )));
    // The organisation is Shonin at its home, not the app's old origin.
    assert!(html.contains("\"url\":\"https://shonin.fi\""));
}

#[test]
fn the_policy_allows_only_this_origin_and_cloudflare_web_analytics() {
    let headers = text(&files(), "_headers");
    let csp = headers
        .lines()
        .find_map(|line| line.trim().strip_prefix("Content-Security-Policy: "))
        .expect("a Content-Security-Policy");
    assert_eq!(
        csp,
        "default-src 'none'; script-src 'self' https://static.cloudflareinsights.com; \
         style-src 'self'; img-src 'self'; font-src 'self'; \
         connect-src https://cloudflareinsights.com/cdn-cgi/rum; manifest-src 'self'; \
         base-uri 'none'; form-action 'none'; frame-ancestors 'none'; upgrade-insecure-requests"
    );
    for header in [
        "X-Content-Type-Options: nosniff",
        "X-Frame-Options: DENY",
        "Referrer-Policy: strict-origin-when-cross-origin",
        "Permissions-Policy: ",
        "Cross-Origin-Opener-Policy: same-origin-allow-popups",
    ] {
        assert!(headers.contains(header), "_headers has no {header}");
    }
    assert!(headers.contains("/assets/*\n  Cache-Control: public, max-age=31536000, immutable\n"));
}

#[test]
fn the_notice_carries_no_wallet_or_api_code() {
    let files = files();
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
    }
}

#[test]
fn the_marketing_site_is_untouched_by_the_notice() {
    let site: BTreeMap<String, Vec<u8>> = marketing::site(&links())
        .unwrap()
        .into_iter()
        .map(|file| (file.path, file.bytes))
        .collect();
    // None of the notice's files or markup reaches shonin.fi.
    assert!(!site.keys().any(|path| path.contains("shutdown")));
    for (path, bytes) in &site {
        if path.ends_with(".html") {
            let html = String::from_utf8(bytes.clone()).unwrap();
            assert!(!html.contains("notice-hero"), "{path}");
            assert!(!html.contains("noindex") || path == "404.html", "{path}");
            assert!(
                html.contains("<script src=\"/assets/site."),
                "{path} runs the site's script"
            );
        }
    }
    // shonin.fi keeps its own redirects, sitemap and crawling, and a policy that runs
    // its own script and the hashed boot script, which the notice has neither of.
    assert!(site.contains_key("_redirects"));
    assert!(site.contains_key("sitemap.xml"));
    let robots = String::from_utf8(site["robots.txt"].clone()).unwrap();
    assert!(robots.contains("Allow: /"));
    let headers = String::from_utf8(site["_headers"].clone()).unwrap();
    let script_src = format!(
        "script-src 'self' {} https://static.cloudflareinsights.com;",
        marketing::boot_hash()
    );
    assert!(headers.contains(&script_src));
    let notice_headers = text(&files(), "_headers");
    assert!(!notice_headers.contains(&marketing::boot_hash()));
}
