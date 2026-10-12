//! Build-time pre-render, run natively after `trunk build`:
//!
//! ```text
//! seo --dist .trunk-dist/app --base-url https://app.shonin.fi \
//!     --app-api https://testnet-api.shonin.fi/api --chains config/chains.toml \
//!     [--api https://testnet-api.shonin.fi/api]
//! ```
//!
//! The app is a wasm single-page app, so the HTML Trunk writes has no content
//! of its own. From one snapshot (`--api`), this writes, into the dist:
//!
//! - one HTML file per page (`agents.html`, `agents/base-1/review.html`,
//!   `status.html`, …, and `404.html`), each with its title, description,
//!   canonical URL and Open Graph tags, and the page itself drawn from the
//!   same views the app draws, so it reads with JavaScript off. The app
//!   replaces it with the live page when it boots. Pages that shouldn't be
//!   listed still get a file, marked `noindex`.
//! - `sitemap.xml` and `robots.txt`.
//! - `_headers` with its Content-Security-Policy filled in: `'self'`, the
//!   API origin the app was built with, and the public RPC origins in
//!   `config/chains.toml`. Trunk's inline loader script is moved into a file
//!   first, so scripts need nothing but `'self'` and `'wasm-unsafe-eval'`.
//!
//! Without `--api` only the pages that need no data are written.
//!
//! Files are flat (`status.html`, not `status/index.html`): Workers static
//! assets serve them at `/status` with no trailing-slash redirect.

// The pre-render is a native tool; built for the browser it does nothing.
#[cfg(target_arch = "wasm32")]
fn main() {}

#[cfg(not(target_arch = "wasm32"))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    native::main()
}

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use std::{collections::BTreeSet, error::Error, fs, path::PathBuf};

    use app::{api, route::Route, shell::shell, store, view::escape};

    const SITE_NAME: &str = "Shonin";

    // Both markers are literals in index.html. If either goes missing this fails
    // loudly rather than shipping the bare shell on every route.
    const HEAD_START: &str = "<!-- seo:head -->";
    const HEAD_END: &str = "<!-- /seo:head -->";
    const BODY: &str = "<!-- seo:body -->";
    /// The line in `_headers` this fills in.
    const CSP_MARKER: &str = "Content-Security-Policy: __CSP__";

    struct Args {
        dist: PathBuf,
        base_url: String,
        app_api: String,
        chains: PathBuf,
        api: Option<String>,
    }

    pub fn main() -> Result<(), Box<dyn Error>> {
        let args = parse_args()?;
        let base = args.base_url.trim_end_matches('/');

        // One snapshot, so every page is written from the same moment.
        if let Some(source) = &args.api {
            let snapshot = api::fetch_snapshot(source)?;
            println!(
                "read {} agents and {} events from {source}",
                snapshot.strategies.len(),
                snapshot.feed.len()
            );
            let store = store::get();
            for info in &snapshot.chains {
                if let Ok(config) = api::fetch_config(source, info.chain) {
                    store.apply_config(&config);
                }
            }
            store.apply(&snapshot);
            // The pre-render has no clock of its own: it says the figures are as
            // fresh as the snapshot, which they are.
            store.sync.last_ok.set(Some(0.0));
        } else {
            println!("no --api: writing only the pages that need no data");
        }

        let index = args.dist.join("index.html");
        let shell_html = fs::read_to_string(&index)
            .map_err(|e| format!("reading {} (run trunk build first): {e}", index.display()))?;
        let shell_html = move_inline_scripts(&args.dist, &shell_html)?;
        fs::write(&index, &shell_html)?;
        for marker in [HEAD_START, HEAD_END, BODY] {
            if shell_html.matches(marker).count() != 1 {
                return Err(
                    format!("marker `{marker}` must appear exactly once in index.html").into(),
                );
            }
        }

        let mut written = Vec::new();
        for route in Route::all_pages().iter().chain([&Route::NotFound]) {
            let Some(file) = route.file() else {
                println!("skipping {:?}: it has no file of its own", route.path());
                continue;
            };
            let html = render(&shell_html, base, route);
            let target = args.dist.join(&file);
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&target, html).map_err(|e| format!("writing {}: {e}", target.display()))?;
            written.push(route.clone());
        }

        let urls: String = written
            .iter()
            .filter(|route| route.is_indexable())
            .filter_map(Route::path)
            .map(|path| {
                format!(
                    "  <url><loc>{}</loc></url>\n",
                    escape(&format!("{base}{path}"))
                )
            })
            .collect();
        fs::write(
            args.dist.join("sitemap.xml"),
            format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">\n{urls}</urlset>\n"
            ),
        )?;
        fs::write(
            args.dist.join("robots.txt"),
            format!("User-agent: *\nAllow: /\n\nSitemap: {base}/sitemap.xml\n"),
        )?;

        write_csp(&args)?;

        println!(
            "pre-rendered {} pages (with 404.html), sitemap.xml, robots.txt and the CSP",
            written.len()
        );
        Ok(())
    }

    fn parse_args() -> Result<Args, Box<dyn Error>> {
        let (mut dist, mut base_url, mut app_api, mut chains, mut api) =
            (None, None, None, None, None);
        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--dist" => dist = args.next().map(PathBuf::from),
                "--base-url" => base_url = args.next(),
                "--app-api" => app_api = args.next(),
                "--chains" => chains = args.next().map(PathBuf::from),
                "--api" => api = args.next().filter(|url| !url.is_empty()),
                other => return Err(format!("unknown argument `{other}`").into()),
            }
        }
        match (dist, base_url, app_api, chains) {
            (Some(dist), Some(base_url), Some(app_api), Some(chains)) => Ok(Args {
                dist,
                base_url,
                app_api,
                chains,
                api,
            }),
            _ => Err("usage: seo --dist <dir> --base-url <url> --app-api <url> --chains <chains.toml> [--api <url>]".into()),
        }
    }

    fn render(shell_html: &str, base: &str, route: &Route) -> String {
        let (before, rest) = shell_html.split_once(HEAD_START).expect("checked in main");
        let (_, after) = rest.split_once(HEAD_END).expect("checked in main");
        let body: app::view::View = shell(route, false).into();
        format!("{before}{}{after}", head(base, route)).replacen(BODY, &body.html(), 1)
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
                let url = escape(&format!("{base}{path}"));
                out += &format!(
                    "<link rel=\"canonical\" href=\"{url}\" />\n<meta property=\"og:url\" content=\"{url}\" />\n"
                );
            }
            _ => out += "<meta name=\"robots\" content=\"noindex\" />\n",
        }
        out
    }

    /// Move every inline `<script>` Trunk wrote (its module loader) into a file of
    /// its own, named by a hash of its contents, so the policy needs no
    /// `'unsafe-inline'` and no per-build hash.
    fn move_inline_scripts(dist: &std::path::Path, html: &str) -> Result<String, Box<dyn Error>> {
        let mut out = String::with_capacity(html.len());
        let mut rest = html;
        while let Some(start) = rest.find("<script") {
            let open_end = rest[start..].find('>').ok_or("an unclosed <script> tag")? + start;
            let tag = &rest[start..=open_end];
            let close = rest[open_end..]
                .find("</script>")
                .ok_or("a <script> with no end")?
                + open_end;
            let body = &rest[open_end + 1..close];
            let inline = !tag.contains(" src=") && !body.trim().is_empty();
            let data = tag.contains("application/ld+json") || tag.contains("application/json");
            out.push_str(&rest[..start]);
            if inline && !data {
                let name = format!("boot-{}.js", short_digest(body));
                fs::write(dist.join(&name), body.trim())?;
                let module = if tag.contains("type=\"module\"") {
                    " type=\"module\""
                } else {
                    ""
                };
                out.push_str(&format!("<script{module} src=\"/{name}\"></script>"));
            } else {
                out.push_str(&rest[start..close + "</script>".len()]);
            }
            rest = &rest[close + "</script>".len()..];
        }
        out.push_str(rest);
        Ok(out)
    }

    fn short_digest(text: &str) -> String {
        use sha2::{Digest, Sha256};
        let digest = Sha256::digest(text.as_bytes());
        digest
            .iter()
            .take(8)
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    /// Fill in `_headers`' Content-Security-Policy.
    fn write_csp(args: &Args) -> Result<(), Box<dyn Error>> {
        let path = args.dist.join("_headers");
        let headers =
            fs::read_to_string(&path).map_err(|e| format!("reading {}: {e}", path.display()))?;
        if headers.matches(CSP_MARKER).count() != 1 {
            return Err(format!("`{CSP_MARKER}` must appear exactly once in _headers").into());
        }
        let mut connect: BTreeSet<String> = BTreeSet::new();
        if let Some(origin) = api::origin(&args.app_api) {
            connect.insert(origin);
        }
        connect.extend(rpc_origins(&fs::read_to_string(&args.chains)?)?);
        let policy = csp(&connect);
        fs::write(
            &path,
            headers.replace(CSP_MARKER, &format!("Content-Security-Policy: {policy}")),
        )?;
        println!("Content-Security-Policy: {policy}");
        Ok(())
    }

    /// The policy: everything from this origin; WebAssembly compiled; data only
    /// from the API and the chains' public endpoints; plus Cloudflare's
    /// cookieless Web Analytics script and beacon.
    fn csp(connect: &BTreeSet<String>) -> String {
        // Cloudflare Web Analytics (cookieless) is injected at the zone: its
        // script and the beacon's report endpoint, and nothing broader.
        let mut connect_src = String::from("'self' https://cloudflareinsights.com");
        for origin in connect {
            connect_src.push(' ');
            connect_src.push_str(origin);
        }
        format!(
            "default-src 'self'; script-src 'self' 'wasm-unsafe-eval' https://static.cloudflareinsights.com; style-src 'self'; font-src 'self'; \
             img-src 'self' data:; connect-src {connect_src}; base-uri 'self'; form-action 'self'; \
             frame-ancestors 'none'; object-src 'none'"
        )
    }

    /// Every public (https or wss) RPC origin in `config/chains.toml`. Local
    /// endpoints are left out: a deployed page can never reach them.
    fn rpc_origins(chains_toml: &str) -> Result<BTreeSet<String>, Box<dyn Error>> {
        let config: toml::Table = chains_toml.parse()?;
        let mut origins = BTreeSet::new();
        if let Some(chains) = config.get("chains").and_then(toml::Value::as_table) {
            for chain in chains.values() {
                for key in ["rpc", "ws", "logs_rpc"] {
                    let Some(urls) = chain.get(key).and_then(toml::Value::as_str) else {
                        continue;
                    };
                    for url in urls.split_whitespace() {
                        if (url.starts_with("https://") || url.starts_with("wss://"))
                            && let Some(origin) = api::origin(url)
                        {
                            origins.insert(origin);
                        }
                    }
                }
            }
        }
        Ok(origins)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn the_policy_allows_no_inline_script_and_only_named_origins() {
            let connect: BTreeSet<String> = ["https://testnet-api.shonin.fi".to_string()].into();
            let policy = csp(&connect);
            assert!(policy.contains(
                "script-src 'self' 'wasm-unsafe-eval' https://static.cloudflareinsights.com;"
            ));
            assert!(policy.contains(
                "connect-src 'self' https://cloudflareinsights.com https://testnet-api.shonin.fi;"
            ));
            assert!(policy.contains("style-src 'self';"));
            assert!(!policy.contains("unsafe-inline"));
            assert!(policy.contains("frame-ancestors 'none'"));
        }

        #[test]
        fn only_public_rpc_endpoints_reach_the_policy() {
            let toml = r#"
                [chains."evm:31337"]
                rpc = "http://127.0.0.1:19100"
                [chains."evm:84532"]
                rpc = "https://sepolia.base.org https://base-sepolia.drpc.org/path"
                [chains."solana:devnet"]
                rpc = "https://api.devnet.solana.com"
                ws = "wss://api.devnet.solana.com"
            "#;
            let origins = rpc_origins(toml).unwrap();
            assert_eq!(
                origins.into_iter().collect::<Vec<_>>(),
                [
                    "https://api.devnet.solana.com",
                    "https://base-sepolia.drpc.org",
                    "https://sepolia.base.org",
                    "wss://api.devnet.solana.com",
                ]
            );
        }

        #[test]
        fn an_inline_loader_moves_into_a_file_and_data_blocks_stay() {
            let dir = std::env::temp_dir().join(format!("shonin-seo-{}", std::process::id()));
            fs::create_dir_all(&dir).unwrap();
            let html = "<head><script type=\"module\">import init from '/app.js';</script>\
                        <script type=\"application/ld+json\">{}</script>\
                        <script src=\"/x.js\"></script></head>";
            let out = move_inline_scripts(&dir, html).unwrap();
            let _ = fs::remove_dir_all(&dir);
            assert!(!out.contains("import init"));
            assert!(out.contains("<script type=\"module\" src=\"/boot-"));
            assert!(out.contains("<script type=\"application/ld+json\">{}</script>"));
            assert!(out.contains("<script src=\"/x.js\"></script>"));
        }
    }
}
