//! The marketing site, shonin.fi: static pages generated from Rust.
//!
//! No wasm, no data fetching, no API, no wallet: every page is complete HTML
//! that reads the same with JavaScript off. One small script adds the motion
//! and the few controls that need it (the phone menu, the questions
//! accordion, the publishing steps, a copy button); a tiny inline one, hashed
//! in the Content-Security-Policy, decides before first paint whether motion
//! runs at all.
//!
//! [`site`] renders every file the site serves; [`write`] puts them on disk.

pub mod html;
mod icons;
pub mod pages;
mod tokens;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::Path;

use anyhow::{Context as _, Result};
use sha2::{Digest, Sha256};

pub use html::Links;

/// One file the site serves, at `path` under the site root.
pub struct File {
    pub path: String,
    pub bytes: Vec<u8>,
}

/// The marketing styles: atoms in the design's `sh-*` names, then the pages.
const CSS: &str = include_str!("../assets/site.css");
/// The motion and the interactive controls.
const JS: &str = include_str!("../assets/site.js");

/// Runs before first paint: motion is on only when the reader hasn't asked
/// for less of it and the browser can observe what is on screen. If the main
/// script never arrives, motion is switched off again so nothing stays hidden.
const BOOT: &str = "(function(d){var e=d.documentElement;if(!matchMedia('(prefers-reduced-motion: reduce)').matches&&'IntersectionObserver' in window){e.classList.add('motion');setTimeout(function(){if(!window.shoninSite){e.classList.remove('motion')}},4000)}})(document);";

struct Asset {
    key: &'static str,
    dir: &'static str,
    stem: &'static str,
    ext: &'static str,
    bytes: &'static [u8],
}

const ASSETS: &[Asset] = &[
    Asset {
        key: "font-sans",
        dir: "fonts",
        stem: "HankenGrotesk-Variable",
        ext: "woff2",
        bytes: include_bytes!("../../ui/fonts/HankenGrotesk-Variable.woff2"),
    },
    Asset {
        key: "font-sans-italic",
        dir: "fonts",
        stem: "HankenGrotesk-Variable-Italic",
        ext: "woff2",
        bytes: include_bytes!("../../ui/fonts/HankenGrotesk-Variable-Italic.woff2"),
    },
    Asset {
        key: "font-display",
        dir: "fonts",
        stem: "PlusJakartaSans-Variable",
        ext: "woff2",
        bytes: include_bytes!("../../ui/fonts/PlusJakartaSans-Variable.woff2"),
    },
    Asset {
        key: "font-serif",
        dir: "fonts",
        stem: "Newsreader-Variable",
        ext: "woff2",
        bytes: include_bytes!("../../ui/fonts/Newsreader-Variable.woff2"),
    },
    Asset {
        key: "font-serif-italic",
        dir: "fonts",
        stem: "Newsreader-Variable-Italic",
        ext: "woff2",
        bytes: include_bytes!("../../ui/fonts/Newsreader-Variable-Italic.woff2"),
    },
    Asset {
        key: "font-mono",
        dir: "fonts",
        stem: "JetBrainsMono-Variable",
        ext: "woff2",
        bytes: include_bytes!("../../ui/fonts/JetBrainsMono-Variable.woff2"),
    },
    Asset {
        key: "mark",
        dir: "logos",
        stem: "shonin-mark",
        ext: "svg",
        bytes: include_bytes!("../../ui/logos/shonin-mark.svg"),
    },
    Asset {
        key: "lockup-reversed",
        dir: "logos",
        stem: "shonin-lockup-reversed",
        ext: "svg",
        bytes: include_bytes!("../assets/logos/shonin-lockup-reversed.svg"),
    },
    Asset {
        key: "tagline-reversed",
        dir: "logos",
        stem: "shonin-lockup-tagline-reversed",
        ext: "svg",
        bytes: include_bytes!("../assets/logos/shonin-lockup-tagline-reversed.svg"),
    },
    // The four chains' own marks, from their official brand kits (see
    // assets/chains/SOURCES.md), in the variant each allows on navy.
    Asset {
        key: "chain-hyperliquid",
        dir: "chains",
        stem: "hyperliquid",
        ext: "svg",
        bytes: include_bytes!("../assets/chains/hyperliquid.svg"),
    },
    Asset {
        key: "chain-solana",
        dir: "chains",
        stem: "solana",
        ext: "svg",
        bytes: include_bytes!("../assets/chains/solana.svg"),
    },
    Asset {
        key: "chain-base",
        dir: "chains",
        stem: "base",
        ext: "svg",
        bytes: include_bytes!("../assets/chains/base.svg"),
    },
    Asset {
        key: "chain-robinhood-chain",
        dir: "chains",
        stem: "robinhood-chain",
        ext: "svg",
        bytes: include_bytes!("../assets/chains/robinhood-chain.svg"),
    },
];

/// Every file the site serves.
pub fn site(links: &Links) -> Result<Vec<File>> {
    let mut files = Vec::new();
    let mut assets: BTreeMap<&'static str, String> = BTreeMap::new();
    for asset in ASSETS {
        let path = format!(
            "assets/{}/{}.{}.{}",
            asset.dir,
            asset.stem,
            fingerprint(asset.bytes),
            asset.ext
        );
        assets.insert(asset.key, format!("/{path}"));
        files.push(File {
            path,
            bytes: asset.bytes.to_vec(),
        });
    }

    let pages = pages::all();
    let css = stylesheet(&assets, &pages)?;
    let css_path = format!("assets/site.{}.css", fingerprint(css.as_bytes()));
    let js_path = format!("assets/site.{}.js", fingerprint(JS.as_bytes()));
    let theme = token_value("hero-ground")?;

    let cx = html::Cx {
        links,
        assets: &assets,
        stylesheet: &format!("/{css_path}"),
        script: &format!("/{js_path}"),
        boot: BOOT,
        theme_color: &theme,
    };
    for (file, page) in &pages {
        files.push(File {
            path: (*file).to_string(),
            bytes: html::document(page, &cx)
                .with_context(|| format!("rendering {file}"))?
                .into_bytes(),
        });
    }
    files.push(File {
        path: css_path,
        bytes: css.into_bytes(),
    });
    files.push(File {
        path: js_path,
        bytes: JS.as_bytes().to_vec(),
    });
    files.push(File {
        path: "_headers".into(),
        bytes: headers().into_bytes(),
    });
    files.push(File {
        path: "_redirects".into(),
        bytes: redirects(links).into_bytes(),
    });
    files.push(File {
        path: "robots.txt".into(),
        bytes: ROBOTS.as_bytes().to_vec(),
    });
    Ok(files)
}

/// Replace whatever is at `out` with the site.
pub fn write(files: &[File], out: &Path) -> Result<()> {
    if out.exists() {
        std::fs::remove_dir_all(out).with_context(|| format!("clearing {}", out.display()))?;
    }
    for file in files {
        let path = out.join(&file.path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("making {}", parent.display()))?;
        }
        std::fs::write(&path, &file.bytes)
            .with_context(|| format!("writing {}", path.display()))?;
    }
    Ok(())
}

/// The first ten hex digits of the content's SHA-256: a new file name for
/// every change, so assets can be cached for good.
fn fingerprint(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))[..10].to_string()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

/// The tokens, the self-hosted faces, the marketing styles, and one
/// `animation-delay` rule for every `dl-<ms>` class the pages use (classes,
/// not inline styles, so the CSP needs no `'unsafe-inline'`).
fn stylesheet(
    assets: &BTreeMap<&'static str, String>,
    pages: &[(&str, html::Page)],
) -> Result<String> {
    let mut css = tokens::css()?;
    let faces = [
        ("Hanken Grotesk", "normal", "100 900", "font-sans"),
        ("Hanken Grotesk", "italic", "100 900", "font-sans-italic"),
        ("Plus Jakarta Sans", "normal", "200 800", "font-display"),
        ("Newsreader", "normal", "200 800", "font-serif"),
        ("Newsreader", "italic", "200 800", "font-serif-italic"),
        ("JetBrains Mono", "normal", "100 800", "font-mono"),
    ];
    for (family, style, weight, key) in faces {
        let url = assets.get(key).context("a font asset is missing")?;
        let _ = writeln!(
            css,
            "@font-face{{font-family:\"{family}\";font-style:{style};font-weight:{weight};font-display:swap;src:url(\"{url}\") format(\"woff2\");}}"
        );
    }
    css.push_str(CSS);

    let mut delays = BTreeSet::new();
    let sources = pages.iter().map(|(_, page)| page.main).chain([html::MENU]);
    for source in sources {
        for (at, _) in source.match_indices("dl-") {
            let digits: String = source[at + 3..]
                .chars()
                .take_while(char::is_ascii_digit)
                .collect();
            if let Ok(ms) = digits.parse::<u32>() {
                delays.insert(ms);
            }
        }
    }
    css.push_str("\n/* Animation delays, one per dl-<ms> class the pages use. */\n");
    for ms in delays {
        let _ = writeln!(css, ".dl-{ms}{{animation-delay:{ms}ms}}");
    }
    Ok(css)
}

/// A colour token's value, for the one place CSS can't reach: `theme-color`.
fn token_value(name: &str) -> Result<String> {
    let json: serde_json::Value = serde_json::from_str(ui::TOKENS)?;
    json.pointer("/color/tokens")
        .and_then(serde_json::Value::as_array)
        .and_then(|tokens| {
            tokens
                .iter()
                .find(|token| token.get("name").and_then(|n| n.as_str()) == Some(name))
        })
        .and_then(|token| token.get("value").and_then(|v| v.as_str()))
        .map(str::to_string)
        .with_context(|| format!("there is no colour token {name}"))
}

/// The SHA-256 of the inline boot script, as a CSP source.
pub fn boot_hash() -> String {
    format!("'sha256-{}'", base64(&Sha256::digest(BOOT.as_bytes())))
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Where Cloudflare Web Analytics loads its beacon from. Cloudflare adds the
/// script to every page at the edge (it is never in the build); it is
/// cookieless and reports aggregate page views and load times.
pub const ANALYTICS_SCRIPT: &str = "https://static.cloudflareinsights.com";
/// Where the beacon reports, and the only place a page may send anything.
pub const ANALYTICS_REPORT: &str = "https://cloudflareinsights.com/cdn-cgi/rum";

/// Workers static assets read these from the output root. The policy allows
/// only this origin, plus the hashed boot script and Cloudflare Web
/// Analytics (its script's host, and its one reporting endpoint): no inline
/// styles, no other hosts, no frames, no forms.
fn headers() -> String {
    let csp = [
        "default-src 'none'".to_string(),
        format!("script-src 'self' {} {ANALYTICS_SCRIPT}", boot_hash()),
        "style-src 'self'".into(),
        "img-src 'self'".into(),
        "font-src 'self'".into(),
        format!("connect-src {ANALYTICS_REPORT}"),
        "manifest-src 'self'".into(),
        "base-uri 'none'".into(),
        "form-action 'none'".into(),
        "frame-ancestors 'none'".into(),
        "upgrade-insecure-requests".into(),
    ]
    .join("; ");
    format!(
        "/*
  Content-Security-Policy: {csp}
  X-Content-Type-Options: nosniff
  X-Frame-Options: DENY
  Referrer-Policy: strict-origin-when-cross-origin
  Permissions-Policy: accelerometer=(), camera=(), geolocation=(), gyroscope=(), magnetometer=(), microphone=(), payment=(), usb=()
  Cross-Origin-Opener-Policy: same-origin
/assets/*
  Cache-Control: public, max-age=31536000, immutable
"
    )
}

/// shonin.fi used to serve the app, so its old addresses go, permanently, to
/// their home at the app's origin: the app's current routes as they are, and
/// the routes it has since renamed straight to their new names (one hop, not
/// two). An old Author page was keyed by a display name that can't become an
/// address here, so it goes to the full list. `/authors` itself is this
/// site's For Authors page, and anything else that isn't a file is the 404.
///
/// Workers read the first rule that matches, so the rules for single
/// addresses come first, then the deeper ones, each before its catch-all.
fn redirects(links: &Links) -> String {
    let app = &links.app;
    let rules: &[(&str, String)] = &[
        // Single addresses.
        ("/agents", format!("{app}/agents")),
        ("/portfolio", format!("{app}/portfolio")),
        ("/publish", format!("{app}/publish")),
        ("/status", format!("{app}/status")),
        ("/you", format!("{app}/you")),
        ("/strategies", format!("{app}/agents")),
        ("/queue", format!("{app}/publish")),
        ("/verify", format!("{app}/status")),
        // The old About page is this site's home.
        ("/about", "/".into()),
        // Renamed pages under an agent, then the rest of the app's routes.
        ("/strategies/:slug/*", format!("{app}/agents/:slug")),
        ("/strategies/:slug", format!("{app}/agents/:slug")),
        ("/agents/:slug/code", format!("{app}/agents/:slug/verify")),
        ("/agents/:slug/record", format!("{app}/agents/:slug")),
        ("/agents/:slug/health", format!("{app}/agents/:slug")),
        ("/agents/*", format!("{app}/agents/:splat")),
        ("/portfolio/*", format!("{app}/portfolio/:splat")),
        ("/publish/*", format!("{app}/publish/:splat")),
        ("/you/*", format!("{app}/you/:splat")),
        ("/queue/:id", format!("{app}/publish/jobs/:id")),
        ("/authors/:handle/*", format!("{app}/agents")),
        ("/authors/:handle", format!("{app}/agents")),
    ];
    let mut out = String::new();
    for (from, to) in rules {
        let _ = writeln!(out, "{from} {to} 301");
    }
    out
}

/// Not for indexing: whether this hostname is indexed is decided per host.
const ROBOTS: &str = "User-agent: *\nDisallow: /\n";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_standard_alphabet_and_padding() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }
}
