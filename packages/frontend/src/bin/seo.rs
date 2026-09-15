//! Build-time SEO for the static site. Runs natively, after `trunk build`:
//!
//! ```text
//! seo --dist .trunk-dist/frontend --base-url https://shonin.fi
//! ```
//!
//! The app is a wasm SPA, so the HTML Trunk writes has no content of its own:
//! crawlers that don't run JavaScript, and every link-preview bot, would see the
//! same empty page on every URL. From the routes in `route.rs`, this writes:
//!
//! - one HTML file per route (`index.html`, `about.html`, ..., and `404.html`)
//!   with that route's title, description, canonical URL and Open Graph tags,
//!   and its copy already in the page. The app replaces that copy when it
//!   boots, with the same content, so nothing is hidden from anyone.
//! - `sitemap.xml`, listing every indexable route.
//! - `robots.txt`, pointing at the sitemap.
//!
//! Files are flat (`about.html`, not `about/index.html`): Workers static assets
//! serve them at `/about` with no trailing-slash redirect.

use std::{error::Error, fs, path::PathBuf};

use frontend::route::{Page, Route};

const SITE_NAME: &str = "Shonin";
const LOGO: &str = "/public/logo.svg";

// Both markers are literals in index.html. If either goes missing this fails
// loudly rather than shipping the generic shell on every route.
const HEAD_START: &str = "<!-- seo:head -->";
const HEAD_END: &str = "<!-- /seo:head -->";
const BODY: &str = "<!-- seo:body -->";

fn main() -> Result<(), Box<dyn Error>> {
    let (dist, base_url) = parse_args()?;
    let base = base_url.trim_end_matches('/');

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

    for &route in Route::INDEXABLE.iter().chain([&Route::NotFound]) {
        let html = render(&shell, base, route);
        let file = match route.path() {
            Some("/") => "index.html".to_string(),
            Some(path) => format!("{}.html", path.trim_start_matches('/')),
            None => "404.html".to_string(),
        };
        let target = dist.join(&file);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&target, html)?;
        println!("  {:<14} {}", file, route.page().title);
    }

    let urls: String = Route::INDEXABLE
        .iter()
        .filter_map(|route| route.path())
        .map(|path| {
            format!(
                "  <url><loc>{}</loc></url>\n",
                escape(&absolute(base, path))
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
        "SEO: {} indexable routes, 404.html, sitemap.xml, robots.txt",
        Route::INDEXABLE.len()
    );
    Ok(())
}

fn parse_args() -> Result<(PathBuf, String), Box<dyn Error>> {
    let mut dist = None;
    let mut base_url = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--dist" => dist = args.next().map(PathBuf::from),
            "--base-url" => base_url = args.next(),
            other => return Err(format!("unknown argument `{other}`").into()),
        }
    }
    match (dist, base_url) {
        (Some(dist), Some(base_url)) => Ok((dist, base_url)),
        _ => Err("usage: seo --dist <dir> --base-url <url>".into()),
    }
}

fn render(shell: &str, base: &str, route: Route) -> String {
    let (before, rest) = shell.split_once(HEAD_START).expect("checked in main");
    let (_, after) = rest.split_once(HEAD_END).expect("checked in main");
    format!("{before}{}{after}", head(base, route)).replacen(BODY, &body(route), 1)
}

fn head(base: &str, route: Route) -> String {
    let Page {
        title, description, ..
    } = route.page();
    let (title, description) = (escape(title), escape(description));
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
        Some(path) => {
            let url = escape(&absolute(base, path));
            out += &format!(
                "<link rel=\"canonical\" href=\"{url}\" />\n\
                 <meta property=\"og:url\" content=\"{url}\" />\n"
            );
        }
        None => out += "<meta name=\"robots\" content=\"noindex\" />\n",
    }
    if route == Route::Home {
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

/// The route's copy, in the same markup the app renders, so the page looks the
/// same before and after the app boots.
fn body(route: Route) -> String {
    let page = route.page();
    let mut out = format!(
        "<a href=\"/\" aria-label=\"{SITE_NAME} home\"><img class=\"logo\" src=\"{LOGO}\" alt=\"\" /></a>\n\
         <h1>{}</h1>\n",
        escape(page.heading)
    );
    for paragraph in page.body {
        out += &format!("<p>{}</p>\n", escape(paragraph));
    }
    if let Some(note) = page.note {
        out += &format!("<span class=\"soon\">{}</span>\n", escape(note));
    }
    out += "<nav>";
    for &link in Route::INDEXABLE {
        let path = link.path().expect("indexable routes have a path");
        let current = if link == route {
            " aria-current=\"page\""
        } else {
            ""
        };
        out += &format!("<a href=\"{path}\"{current}>{}</a>", escape(link.label()));
    }
    out += "</nav>\n";
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
