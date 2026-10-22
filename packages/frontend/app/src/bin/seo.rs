//! Build-time pre-render, run natively after `trunk build`:
//!
//! ```text
//! seo --dist .trunk-dist/app --base-url https://app.shonin.fi \
//!     --app-api https://testnet-api.shonin.fi/api --chains config/chains.toml \
//!     --network testnet [--api https://testnet-api.shonin.fi/api]
//! ```
//!
//! The app is a wasm single-page app, so the HTML Trunk writes has no content
//! of its own. From one snapshot (`--api`), this writes, into the dist:
//!
//! - one HTML file per page (`agents.html`, `agents/base-1/review.html`,
//!   `status.html`, …, and `404.html`), each with its title, description,
//!   canonical URL, Open Graph and X card tags (every page shows the app's
//!   share image, `share/shonin-app.png`, which this writes too, from
//!   `packages/frontend/ui/images`), and the page itself drawn from the
//!   same views the app draws, so it reads with JavaScript off. The app
//!   replaces it with the live page when it boots. Pages that shouldn't be
//!   listed still get a file, marked `noindex`.
//! - one shared page per kind of address with an id (`_shells/claim.html`,
//!   `_shells/job.html`, `_shells/activate.html`, `_shells/manage.html`),
//!   and one for `/you`, which is only ever the connected account's own
//!   (`_shells/you.html`): each marked `noindex`, never in the sitemap, and
//!   served by `_redirects` with status 200 at every such address.
//! - `sitemap.xml` and `robots.txt`.
//! - `_headers` with its Content-Security-Policy filled in: `'self'`, the
//!   API origin the app was built with, and the public RPC origins of the
//!   chains `config/chains.toml` lists for the network the build is for
//!   (`--network`), and no other network's. Trunk's inline loader script is moved into a file
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
    /// Where the share image is served. Not fingerprinted: link previews are
    /// cached by address, so it stays put and `_headers` caches it briefly.
    const SHARE_IMAGE: &str = "/share/shonin-app.png";

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
        network: String,
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

        // One shared page per kind of address with an id (a claim, a job,
        // an activation, an Author's controls), and one for the connected
        // account's own agents and jobs: `_redirects` serves it with status
        // 200 at every such address, and the app draws the page from the
        // address (and the wallet) when it boots.
        for route in Route::shell_kinds() {
            let file = route.shell_file().expect("every shell kind has a file");
            let target = args.dist.join(file);
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&target, render_shell(&shell_html, base, &route))
                .map_err(|e| format!("writing {}: {e}", target.display()))?;
        }

        let share = args.dist.join(SHARE_IMAGE.trim_start_matches('/'));
        if let Some(parent) = share.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&share, ui::images::SHARE_APP.png.bytes)
            .map_err(|e| format!("writing {}: {e}", share.display()))?;
        fs::write(args.dist.join("sitemap.xml"), sitemap(base, &written))?;
        fs::write(args.dist.join("robots.txt"), robots(base))?;

        write_csp(&args)?;

        println!(
            "pre-rendered {} pages (with 404.html), the share image, sitemap.xml, robots.txt and the CSP",
            written.len()
        );
        Ok(())
    }

    fn parse_args() -> Result<Args, Box<dyn Error>> {
        let (mut dist, mut base_url, mut app_api, mut chains, mut network, mut api) =
            (None, None, None, None, None, None);
        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--dist" => dist = args.next().map(PathBuf::from),
                "--base-url" => base_url = args.next(),
                "--app-api" => app_api = args.next(),
                "--chains" => chains = args.next().map(PathBuf::from),
                "--network" => network = args.next().filter(|name| !name.is_empty()),
                "--api" => api = args.next().filter(|url| !url.is_empty()),
                other => return Err(format!("unknown argument `{other}`").into()),
            }
        }
        match (dist, base_url, app_api, chains, network) {
            (Some(dist), Some(base_url), Some(app_api), Some(chains), Some(network)) => Ok(Args {
                dist,
                base_url,
                app_api,
                chains,
                network,
                api,
            }),
            _ => Err("usage: seo --dist <dir> --base-url <url> --app-api <url> --chains <chains.toml> --network <name> [--api <url>]".into()),
        }
    }

    fn render(shell_html: &str, base: &str, route: &Route) -> String {
        let (before, rest) = shell_html.split_once(HEAD_START).expect("checked in main");
        let (_, after) = rest.split_once(HEAD_END).expect("checked in main");
        let body: app::view::View = shell(route, false).into();
        format!("{before}{}{after}", head(base, route)).replacen(BODY, &body.html(), 1)
    }

    /// A shared page for every address of `route`'s kind: the frame, the
    /// kind's heading and a placeholder where the page will be, since which
    /// claim, job or agent it is is known only from the address.
    fn render_shell(shell_html: &str, base: &str, route: &Route) -> String {
        let (before, rest) = shell_html.split_once(HEAD_START).expect("checked in main");
        let (_, after) = rest.split_once(HEAD_END).expect("checked in main");
        let title = route.page().title;
        let heading = title.trim_end_matches(" · Shonin").to_string();
        let body = app::view::el("div")
            .class("sh-author")
            .attr("aria-busy", "true")
            .child(app::view::el("h1").class("sh-author__title").text(heading))
            .child(app::atoms::skeleton::skeleton_block("100%", 160))
            .child(
                app::view::el("noscript")
                    .child(app::view::el("p").text("This page needs JavaScript.")),
            );
        let page = app::pages::Page {
            top: None,
            body: body.into(),
        };
        let body: app::view::View = app::shell::frame(route, page).into();
        format!("{before}{}{after}", head(base, route)).replacen(BODY, &body.html(), 1)
    }

    /// Every listed page the pre-render wrote, at its full address.
    fn sitemap(base: &str, written: &[Route]) -> String {
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
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">\n{urls}</urlset>\n"
        )
    }

    /// Everything may be crawled; the sitemap says what there is.
    fn robots(base: &str) -> String {
        format!("User-agent: *\nAllow: /\n\nSitemap: {base}/sitemap.xml\n")
    }

    fn head(base: &str, route: &Route) -> String {
        let page = route.page();
        let (title, description) = (escape(&page.title), escape(&page.description));
        let share = &ui::images::SHARE_APP;
        let image = escape(&format!("{base}{SHARE_IMAGE}"));
        let (width, height, alt) = (share.png.width, share.png.height, escape(share.alt));
        let mut out = format!(
            "<title>{title}</title>\n\
             <meta name=\"description\" content=\"{description}\" />\n\
             <meta property=\"og:type\" content=\"website\" />\n\
             <meta property=\"og:site_name\" content=\"{SITE_NAME}\" />\n\
             <meta property=\"og:title\" content=\"{title}\" />\n\
             <meta property=\"og:description\" content=\"{description}\" />\n\
             <meta property=\"og:image\" content=\"{image}\" />\n\
             <meta property=\"og:image:width\" content=\"{width}\" />\n\
             <meta property=\"og:image:height\" content=\"{height}\" />\n\
             <meta property=\"og:image:alt\" content=\"{alt}\" />\n\
             <meta name=\"twitter:card\" content=\"summary_large_image\" />\n\
             <meta name=\"twitter:image\" content=\"{image}\" />\n\
             <meta name=\"twitter:image:alt\" content=\"{alt}\" />\n"
        );
        out += &ui::social::head_html(app::route::SITE);
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
        connect.extend(rpc_origins(
            &fs::read_to_string(&args.chains)?,
            &args.network,
        )?);
        let policy = csp(&connect);
        fs::write(
            &path,
            headers.replace(CSP_MARKER, &format!("Content-Security-Policy: {policy}")),
        )?;
        println!("Content-Security-Policy: {policy}");
        Ok(())
    }

    /// Where Cloudflare's Web Analytics script is loaded from.
    const ANALYTICS_SCRIPT: &str = "https://static.cloudflareinsights.com";
    /// The one address the analytics beacon reports to.
    const ANALYTICS_REPORT: &str = "https://cloudflareinsights.com/cdn-cgi/rum";

    /// The policy: everything from this origin; WebAssembly compiled; data only
    /// from the API and the chains' public endpoints; plus Cloudflare's
    /// cookieless Web Analytics script and beacon.
    fn csp(connect: &BTreeSet<String>) -> String {
        // Cloudflare Web Analytics (cookieless) is injected at the zone: its
        // script and the beacon's report address, and nothing broader.
        let mut connect_src = format!("'self' {ANALYTICS_REPORT}");
        for origin in connect {
            connect_src.push(' ');
            connect_src.push_str(origin);
        }
        format!(
            "default-src 'self'; script-src 'self' 'wasm-unsafe-eval' {ANALYTICS_SCRIPT}; style-src 'self'; font-src 'self'; \
             img-src 'self' data:; connect-src {connect_src}; base-uri 'self'; form-action 'self'; \
             frame-ancestors 'none'; object-src 'none'"
        )
    }

    /// Every public (https or wss) RPC origin of the chains `network` names
    /// in `config/chains.toml`; another network's chains are left out. Local
    /// endpoints are left out too: a deployed page can never reach them.
    fn rpc_origins(chains_toml: &str, network: &str) -> Result<BTreeSet<String>, Box<dyn Error>> {
        let config: toml::Table = chains_toml.parse()?;
        let keys: BTreeSet<&str> = config
            .get("networks")
            .and_then(|networks| networks.get(network))
            .and_then(toml::Value::as_table)
            .ok_or_else(|| format!("no network `{network}` in the chains config"))?
            .values()
            .filter_map(toml::Value::as_str)
            .filter(|key| key.contains(':'))
            .collect();
        let mut origins = BTreeSet::new();
        if let Some(chains) = config.get("chains").and_then(toml::Value::as_table) {
            for (key, chain) in chains {
                if !keys.contains(key.as_str()) {
                    continue;
                }
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
                "connect-src 'self' https://cloudflareinsights.com/cdn-cgi/rum https://testnet-api.shonin.fi;"
            ));
            assert!(policy.contains("style-src 'self';"));
            assert!(!policy.contains("unsafe-inline"));
            assert!(policy.contains("frame-ancestors 'none'"));
        }

        #[test]
        fn the_analytics_beacon_may_report_only_to_its_one_address() {
            let policy = csp(&BTreeSet::new());
            let connect = policy
                .split(';')
                .map(str::trim)
                .find_map(|directive| directive.strip_prefix("connect-src "))
                .expect("a connect-src");
            assert_eq!(
                connect.split_whitespace().collect::<Vec<_>>(),
                ["'self'", "https://cloudflareinsights.com/cdn-cgi/rum"]
            );
        }

        #[test]
        fn the_policy_names_only_the_endpoints_of_the_network_the_build_is_for() {
            let toml = r#"
                [chains."evm:84532"]
                rpc = "https://sepolia.base.org"
                [chains."evm:8453"]
                rpc = "https://mainnet.base.org"
                [chains."solana:mainnet-beta"]
                rpc = "https://api.mainnet-beta.solana.com"
                [networks.testnet]
                base = "evm:84532"
                root = "base"
                [networks.mainnet]
                base = "evm:8453"
                solana = "solana:mainnet-beta"
            "#;
            assert_eq!(
                rpc_origins(toml, "testnet")
                    .unwrap()
                    .into_iter()
                    .collect::<Vec<_>>(),
                ["https://sepolia.base.org"]
            );
            assert_eq!(
                rpc_origins(toml, "mainnet")
                    .unwrap()
                    .into_iter()
                    .collect::<Vec<_>>(),
                [
                    "https://api.mainnet-beta.solana.com",
                    "https://mainnet.base.org"
                ]
            );
            assert!(rpc_origins(toml, "elsewhere").is_err());
            // The repository's own config: a testnet build names no mainnet
            // endpoint.
            let ours = include_str!("../../../../../config/chains.toml");
            let testnet = rpc_origins(ours, "testnet").unwrap();
            assert!(testnet.contains("https://sepolia.base.org"));
            for mainnet in rpc_origins(ours, "mainnet").unwrap() {
                assert!(!testnet.contains(&mainnet), "{mainnet}");
            }
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
                [networks.testnet]
                local = "evm:31337"
                base = "evm:84532"
                solana = "solana:devnet"
            "#;
            let origins = rpc_origins(toml, "testnet").unwrap();
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
        fn a_shared_page_names_its_kind_once_and_is_never_indexed() {
            let shell_html = format!("<head>{HEAD_START}{HEAD_END}</head><body>{BODY}</body>");
            for route in Route::shell_kinds() {
                let html = render_shell(&shell_html, "https://app.shonin.fi", &route);
                assert_eq!(html.matches("<h1").count(), 1, "{html}");
                assert!(html.contains("<meta name=\"robots\" content=\"noindex\" />"));
                assert!(!html.contains("rel=\"canonical\""));
                assert!(!html.contains(BODY));
            }
        }

        #[test]
        fn your_agents_page_is_a_noindex_shell_and_never_in_the_sitemap() {
            let shell_html = format!("<head>{HEAD_START}{HEAD_END}</head><body>{BODY}</body>");
            let base = "https://app.shonin.fi";
            assert!(Route::shell_kinds().contains(&Route::You));
            let html = render_shell(&shell_html, base, &Route::You);
            assert_eq!(meta(&html, "robots"), Some("noindex"));
            assert!(!html.contains("rel=\"canonical\""));
            assert!(
                html.contains("<title>Your agents · Shonin</title>"),
                "{html}"
            );
            // Only its shell: no agent or job of anyone's is written into it.
            assert!(html.contains("aria-busy=\"true\""));
            let written: Vec<Route> = Route::all_pages().into_iter().chain([Route::You]).collect();
            assert!(!sitemap(base, &written).contains("/you"));
        }

        /// The `content` of the first `<meta>` whose `property` or `name` is
        /// `key`.
        fn meta<'a>(html: &'a str, key: &str) -> Option<&'a str> {
            ["property", "name"].iter().find_map(|attr| {
                let needle = format!("<meta {attr}=\"{key}\" content=\"");
                let at = html.find(&needle)? + needle.len();
                Some(&html[at..at + html[at..].find('"')?])
            })
        }

        #[test]
        fn every_pre_rendered_page_previews_with_the_share_image_at_its_full_address() {
            let shell_html = format!("<head>{HEAD_START}{HEAD_END}</head><body>{BODY}</body>");
            let base = "https://app.shonin.fi";
            let mut pages: Vec<String> = Route::all_pages()
                .iter()
                .chain([&Route::NotFound])
                .map(|route| render(&shell_html, base, route))
                .collect();
            pages.extend(
                Route::shell_kinds()
                    .iter()
                    .map(|route| render_shell(&shell_html, base, route)),
            );
            assert!(pages.len() > 4);
            for html in &pages {
                assert_eq!(
                    meta(html, "og:image"),
                    Some("https://app.shonin.fi/share/shonin-app.png"),
                    "{html}"
                );
                assert_eq!(meta(html, "og:image:width"), Some("1200"));
                assert_eq!(meta(html, "og:image:height"), Some("630"));
                assert!(meta(html, "og:image:alt").is_some_and(|alt| alt.starts_with("Shonin.")));
                assert_eq!(meta(html, "og:site_name"), Some("Shonin"));
                assert_eq!(meta(html, "twitter:card"), Some("summary_large_image"));
                assert_eq!(meta(html, "twitter:image"), meta(html, "og:image"));
            }
            assert_eq!(
                ui::images::dimensions(ui::images::SHARE_APP.png.bytes),
                Some((1200, 630))
            );
            assert!(SHARE_IMAGE.ends_with(ui::images::SHARE_APP.png.name));
        }

        #[test]
        fn every_pre_rendered_page_names_the_x_account_and_every_place_shonin_is() {
            let shell_html = format!("<head>{HEAD_START}{HEAD_END}</head><body>{BODY}</body>");
            let base = "https://app.shonin.fi";
            let mut pages: Vec<String> = Route::all_pages()
                .iter()
                .chain([&Route::NotFound])
                .map(|route| render(&shell_html, base, route))
                .collect();
            pages.extend(
                Route::shell_kinds()
                    .iter()
                    .map(|route| render_shell(&shell_html, base, route)),
            );
            for html in &pages {
                assert_eq!(meta(html, "twitter:site"), Some("@ShoninFi"), "{html}");
                assert_eq!(
                    html.matches("<script type=\"application/ld+json\">")
                        .count(),
                    1
                );
                for place in [
                    "https://x.com/ShoninFi",
                    "https://discord.gg/2pabyT6rYJ",
                    app::route::GITHUB,
                ] {
                    assert!(html.contains(&format!("\"{place}\"")), "{place}");
                }
            }
        }

        #[test]
        fn every_page_is_kept_from_other_sites_windows_but_may_open_wallet_popups() {
            let headers = include_str!("../../_headers");
            assert!(headers.contains("/*\n"));
            assert!(
                headers.contains("  Cross-Origin-Opener-Policy: same-origin-allow-popups\n"),
                "{headers}"
            );
        }

        #[test]
        fn the_loader_never_puts_the_apps_bindings_on_the_window() {
            let index = include_str!("../../index.html");
            let rust = index
                .lines()
                .find(|line| line.contains("data-trunk rel=\"rust\""))
                .expect("the app's build line");
            assert!(rust.contains(" data-wasm-no-import"), "{rust}");
        }

        #[test]
        fn the_404_is_never_listed() {
            let shell_html = format!("<head>{HEAD_START}{HEAD_END}</head><body>{BODY}</body>");
            let html = render(&shell_html, "https://app.shonin.fi", &Route::NotFound);
            assert_eq!(meta(&html, "robots"), Some("noindex"));
            assert!(!html.contains("rel=\"canonical\""));
            assert!(!sitemap("https://app.shonin.fi", &[Route::NotFound]).contains("<loc>"));
        }

        #[test]
        fn robots_allows_everything_and_names_the_sitemap() {
            assert_eq!(
                robots("https://app.shonin.fi"),
                "User-agent: *\nAllow: /\n\nSitemap: https://app.shonin.fi/sitemap.xml\n"
            );
            let sitemap = sitemap(
                "https://app.shonin.fi",
                &[
                    Route::Agents { author: None },
                    Route::Status,
                    Route::Portfolio,
                ],
            );
            assert!(sitemap.contains("<loc>https://app.shonin.fi/agents</loc>"));
            assert!(sitemap.contains("<loc>https://app.shonin.fi/status</loc>"));
            assert!(
                !sitemap.contains("/portfolio"),
                "a private page is not listed"
            );
        }

        #[test]
        fn the_icons_are_linked_copied_and_cached_and_so_is_the_share_image() {
            let index = include_str!("../../index.html");
            assert!(
                index.contains("<link data-trunk rel=\"copy-dir\" href=\"../ui/images/icons\" />")
            );
            for (rel, icon) in [
                (
                    "icon\" type=\"image/png\" sizes=\"32x32",
                    &ui::images::ICON_32,
                ),
                (
                    "icon\" type=\"image/png\" sizes=\"512x512",
                    &ui::images::ICON_512,
                ),
                ("apple-touch-icon", &ui::images::ICON_180),
            ] {
                assert!(
                    index.contains(&format!(
                        "<link rel=\"{rel}\" href=\"/icons/{}\" />",
                        icon.name
                    )),
                    "{}",
                    icon.name
                );
            }
            let headers = include_str!("../../_headers");
            for dir in ["/icons/*", "/share/*"] {
                assert!(
                    headers.contains(&format!("{dir}\n  Cache-Control: public, max-age=86400\n")),
                    "{dir}"
                );
            }
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
