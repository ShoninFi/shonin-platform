//! Build-time pre-render for the static site. Runs natively, after
//! `trunk build`:
//!
//! ```text
//! seo --dist .trunk-dist/app --base-url https://shonin.fi [--api http://localhost:19081]
//! ```
//!
//! The app is a wasm single-page app, so the HTML Trunk writes has no content
//! of its own: crawlers that don't run JavaScript, and every link-preview bot,
//! would see the same empty page on every URL. From the routes in `route.rs`,
//! this writes:
//!
//! - one HTML file per page (`index.html`, `agents.html`,
//!   `agents/funding-basis-v3/record.html`, …, and `404.html`) with that
//!   route's title, description, canonical URL and Open Graph tags, and a
//!   no-JavaScript view of the same data already in the page. The app replaces
//!   that view with the interactive one when it boots. Pages that shouldn't be
//!   listed still get a file, marked `noindex`: without one, opening such a
//!   page directly would answer 404 before the app could render it.
//! - `sitemap.xml`, listing every indexable route.
//! - `robots.txt`, pointing at the sitemap.
//!
//! With `--api`, it reads one snapshot from a running platform first, so every
//! strategy, author and build has a page written for it. Without it, only the
//! pages that need no data are written: a build should not depend on a service
//! being up, and a page of nothing would be worse than no page at all.
//!
//! Files are flat (`about.html`, not `about/index.html`): Workers static
//! assets serve them at `/about` with no trailing-slash redirect, which is
//! also why a nested route becomes `agents/<slug>/record.html`. A name in a
//! path is percent-encoded in links and decoded in the file name, since the
//! host decodes a request before it looks for the file (see `Route::file`).

use std::{error::Error, fs, path::PathBuf};

use app::{prerender, route::Route};

const SITE_NAME: &str = "Shonin";
const LOGO: &str = "/public/logo.svg";

// Both markers are literals in index.html. If either goes missing this fails
// loudly rather than shipping the generic shell on every route.
const HEAD_START: &str = "<!-- seo:head -->";
const HEAD_END: &str = "<!-- /seo:head -->";
const BODY: &str = "<!-- seo:body -->";

fn main() -> Result<(), Box<dyn Error>> {
    let (dist, base_url, api) = parse_args()?;
    let base = base_url.trim_end_matches('/');

    // One snapshot, so every page written below is written from the same
    // moment, exactly as the app renders it.
    if let Some(api) = &api {
        let snapshot = app::api::fetch_snapshot(api)?;
        println!(
            "read {} agents, {} builds and {} authors from {api}",
            snapshot.strategies.len(),
            snapshot.jobs.len(),
            snapshot.authors.len()
        );
        // The terms each chain is running under, so a written page quotes the
        // platform rather than whatever this build was compiled with.
        for info in &snapshot.chains {
            app::data::set_config(app::api::fetch_config(api, info.chain)?);
        }
        app::data::set(snapshot);
    } else {
        println!("no --api: writing only the pages that need no data");
    }

    let shell = fs::read_to_string(dist.join("index.html")).map_err(|e| {
        format!(
            "reading {}/index.html (run trunk build first): {e}",
            dist.display()
        )
    })?;
    for marker in [HEAD_START, HEAD_END, BODY] {
        if shell.matches(marker).count() != 1 {
            return Err(format!("marker `{marker}` must appear exactly once in index.html").into());
        }
    }

    // A route with no file of its own (a name that would leave its
    // directory) is skipped, and kept out of the sitemap with it.
    let mut pages = Route::all_pages();
    pages.retain(|route| {
        let written = route.file().is_some();
        if !written {
            println!(
                "skipping {:?}: its path has no safe file name",
                route.path()
            );
        }
        written
    });
    for route in pages.iter().chain([&Route::NotFound]) {
        let Some(file) = route.file() else {
            continue;
        };
        let html = render(&shell, base, route);
        let target = dist.join(&file);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&target, html)?;
    }

    let listed: Vec<Route> = Route::indexable()
        .into_iter()
        .filter(|route| pages.contains(route))
        .collect();
    let urls: String = listed
        .iter()
        .filter_map(Route::path)
        .map(|path| {
            format!(
                "  <url><loc>{}</loc></url>\n",
                escape(&absolute(base, &path))
            )
        })
        .collect();
    fs::write(
        dist.join("sitemap.xml"),
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">\n{urls}</urlset>\n"
        ),
    )?;

    fs::write(
        dist.join("robots.txt"),
        format!("User-agent: *\nAllow: /\n\nSitemap: {base}/sitemap.xml\n"),
    )?;

    println!(
        "pre-rendered {} pages ({} of them listed), 404.html, sitemap.xml, robots.txt",
        pages.len(),
        listed.len()
    );
    Ok(())
}

fn parse_args() -> Result<(PathBuf, String, Option<String>), Box<dyn Error>> {
    let mut dist = None;
    let mut base_url = None;
    let mut api = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--dist" => dist = args.next().map(PathBuf::from),
            "--base-url" => base_url = args.next(),
            "--api" => api = args.next().filter(|url| !url.is_empty()),
            other => return Err(format!("unknown argument `{other}`").into()),
        }
    }
    match (dist, base_url) {
        (Some(dist), Some(base_url)) => Ok((dist, base_url, api)),
        _ => Err("usage: seo --dist <dir> --base-url <url> [--api <url>]".into()),
    }
}

fn render(shell: &str, base: &str, route: &Route) -> String {
    let (before, rest) = shell.split_once(HEAD_START).expect("checked in main");
    let (_, after) = rest.split_once(HEAD_END).expect("checked in main");
    format!("{before}{}{after}", head(base, route)).replacen(BODY, &prerender::body(route), 1)
}

fn head(base: &str, route: &Route) -> String {
    let page = route.page();
    let (title, description) = (escape(&page.title), escape(&page.description));
    let mut out = format!(
        "<title>{title}</title>\n\
         <meta name=\"description\" content=\"{description}\" />\n\
         <meta property=\"og:type\" content=\"website\" />\n\
         <meta property=\"og:site_name\" content=\"{SITE_NAME}\" />\n\
         <meta property=\"og:title\" content=\"{title}\" />\n\
         <meta property=\"og:description\" content=\"{description}\" />\n\
         <meta name=\"twitter:card\" content=\"summary\" />\n"
    );
    match route.path() {
        Some(path) if route.is_indexable() => {
            let url = escape(&absolute(base, &path));
            out += &format!(
                "<link rel=\"canonical\" href=\"{url}\" />\n\
                 <meta property=\"og:url\" content=\"{url}\" />\n"
            );
        }
        _ => out += "<meta name=\"robots\" content=\"noindex\" />\n",
    }
    if *route == Route::Home {
        // Structured data: tells search engines the site's name and logo.
        out += &format!(
            "<script type=\"application/ld+json\">{{\"@context\":\"https://schema.org\",\"@graph\":[\
             {{\"@type\":\"WebSite\",\"name\":\"{SITE_NAME}\",\"url\":\"{base}/\"}},\
             {{\"@type\":\"Organization\",\"name\":\"{SITE_NAME}\",\"url\":\"{base}/\",\"logo\":\"{base}{LOGO}\"}}\
             ]}}</script>\n"
        );
    }
    out
}

fn absolute(base: &str, path: &str) -> String {
    format!("{base}{path}")
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
