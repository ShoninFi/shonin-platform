//! The page shell every page shares: the document head, the marketing
//! header, the phone menu, the footer, and the small template language the
//! pages are written in.
//!
//! Pages are HTML written out in full, close to the design's own markup, with
//! a handful of placeholders the build fills in:
//!
//! - `{{app}}` and `{{docs}}`: the app's and the docs' origins.
//! - `{{i:name:size}}` (or `{{i:name:size:stroke}}`): an icon.
//! - `{{asset:name}}`: the fingerprinted address of a logo or font.
//! - `{{social}}`: the footer's social links, each with its brand mark.
//!
//! Nothing a page shows comes from outside the build, so there is no
//! untrusted text to escape; the few values that are passed in (the origins)
//! are escaped anyway.

use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};

use crate::icons;

/// Where the links go, and where the site itself lives.
#[derive(Clone, Debug)]
pub struct Links {
    /// The app's origin, with no trailing slash: `https://app.shonin.fi`.
    pub app: String,
    /// The docs' origin, with no trailing slash.
    pub docs: String,
    /// This site's own origin, for canonical addresses.
    pub site: String,
}

/// What the build knows when it renders a page.
pub struct Cx<'a> {
    pub links: &'a Links,
    /// Asset name (`mark`, `lockup-reversed`, `font-sans`...) to its address.
    pub assets: &'a BTreeMap<&'static str, String>,
    pub stylesheet: &'a str,
    pub script: &'a str,
    /// The inline script that runs before first paint, and its CSP hash.
    pub boot: &'a str,
    pub theme_color: &'a str,
    /// The share image's path on this site, such as `/share/shonin.png`.
    pub share_image: &'a str,
}

/// Which page is current, for `aria-current` in the header.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Current {
    Home,
    Authors,
    Other,
}

pub struct Page<'a> {
    /// The route, such as `/` or `/authors`.
    pub path: &'a str,
    pub title: &'a str,
    pub description: &'a str,
    pub current: Current,
    /// A class on `<body>`, for page-level styles.
    pub class: &'a str,
    /// Everything inside `<main>`.
    pub main: &'a str,
    /// The footer: the full band, or the compact one the 404 uses.
    pub footer: Footer,
    /// Whether search engines may list it (and the sitemap names it). Only
    /// the 404 is kept out.
    pub indexed: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Footer {
    Full,
    Compact,
}

pub fn esc(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// Fill in a page's placeholders.
pub fn expand(template: &str, cx: &Cx) -> Result<String> {
    let mut out = String::with_capacity(template.len() + template.len() / 4);
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let end = after.find("}}").with_context(|| {
            format!(
                "an unclosed placeholder near {:?}",
                &after[..after.len().min(40)]
            )
        })?;
        out.push_str(&placeholder(&after[..end], cx)?);
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    Ok(out)
}

fn placeholder(name: &str, cx: &Cx) -> Result<String> {
    if name == "app" {
        return Ok(esc(&cx.links.app));
    }
    if name == "docs" {
        return Ok(esc(&cx.links.docs));
    }
    if name == "social" {
        return Ok(ui::social::Social::row_html(20));
    }
    if let Some(icon) = name.strip_prefix("i:") {
        let mut parts = icon.split(':');
        let icon_name = parts.next().unwrap_or_default();
        let size: u32 = parts
            .next()
            .unwrap_or("24")
            .parse()
            .with_context(|| format!("the size of the icon {icon:?}"))?;
        let stroke = parts.next().unwrap_or("1.75");
        return icons::svg(icon_name, size, stroke)
            .with_context(|| format!("there is no icon called {icon_name:?}"));
    }
    if let Some(asset) = name.strip_prefix("asset:") {
        return cx
            .assets
            .get(asset)
            .cloned()
            .with_context(|| format!("there is no asset called {asset:?}"));
    }
    bail!("an unknown placeholder {{{{{name}}}}}")
}

/// The whole document.
pub fn document(page: &Page, cx: &Cx) -> Result<String> {
    let share = &ui::images::SHARE_SITE;
    // A page that is listed names its one address; the 404 is served at any
    // address, so it names none and asks not to be listed.
    let address = if page.indexed {
        let url = esc(&format!("{}{}", cx.links.site, page.path));
        format!(
            "<link rel=\"canonical\" href=\"{url}\">\n<meta property=\"og:url\" content=\"{url}\">\n"
        )
    } else {
        "<meta name=\"robots\" content=\"noindex\">\n".to_string()
    };
    let head = format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title}</title>
<meta name="description" content="{description}">
<meta name="color-scheme" content="light">
<meta name="theme-color" content="{theme}">
{address}<meta property="og:type" content="website">
<meta property="og:site_name" content="Shonin">
<meta property="og:title" content="{title}">
<meta property="og:description" content="{description}">
<meta property="og:image" content="{image}">
<meta property="og:image:width" content="{width}">
<meta property="og:image:height" content="{height}">
<meta property="og:image:alt" content="{alt}">
<meta name="twitter:card" content="summary_large_image">
<meta name="twitter:image" content="{image}">
<meta name="twitter:image:alt" content="{alt}">
<link rel="icon" type="image/png" sizes="32x32" href="{{{{asset:icon-32}}}}">
<link rel="icon" type="image/png" sizes="512x512" href="{{{{asset:icon-512}}}}">
<link rel="icon" type="image/svg+xml" href="{{{{asset:mark}}}}">
<link rel="apple-touch-icon" href="{{{{asset:icon-180}}}}">
<link rel="preload" href="{{{{asset:font-sans}}}}" as="font" type="font/woff2" crossorigin>
<link rel="preload" href="{{{{asset:font-display}}}}" as="font" type="font/woff2" crossorigin>
<link rel="stylesheet" href="{stylesheet}">
<script>{boot}</script>
<script src="{script}" defer></script>
</head>
"#,
        title = esc(page.title),
        description = esc(page.description),
        theme = esc(cx.theme_color),
        image = esc(&format!("{}{}", cx.links.site, cx.share_image)),
        width = share.png.width,
        height = share.png.height,
        alt = esc(share.alt),
        stylesheet = esc(cx.stylesheet),
        boot = cx.boot,
        script = esc(cx.script),
    );
    let footer = match page.footer {
        Footer::Full => FOOTER,
        Footer::Compact => FOOTER_COMPACT,
    };
    let body = format!(
        r##"<body class="{class}">
<a class="skip" href="#content">Skip to content</a>
<div class="page" id="page">
{header}
<main id="content" tabindex="-1">
{main}
</main>
{footer}
</div>
{menu}
</body>
</html>
"##,
        class = esc(page.class),
        header = header(page.current),
        main = page.main,
        menu = MENU,
    );
    expand(&(head + &body), cx)
}

/// The marketing header: transparent over the hero, and it scrolls away with
/// it. Desktop shows the whole nav; tablet the lockup, Docs, "Open the app"
/// and the menu; phone the lockup and the menu.
fn header(current: Current) -> String {
    let aria = |is: bool| if is { r#" aria-current="page""# } else { "" };
    format!(
        r#"<header class="site-header sh-hero">
  <div class="wrap site-header__bar">
    <a class="site-header__home" href="/" aria-label="Shonin home"><img src="{{{{asset:lockup-reversed}}}}" alt="Shonin" width="93" height="30"></a>
    <nav class="site-nav" aria-label="Main">
      <a class="sh-nav" href="/#how">How it works</a>
      <a class="sh-nav" href="/#traders">For Traders</a>
      <a class="sh-nav" href="/authors"{authors}>For Authors</a>
      <a class="sh-nav" href="/#fees">Fees</a>
      <a class="sh-nav sh-nav--ext" href="{{{{docs}}}}/">Docs{{{{i:external:16}}}}</a>
    </nav>
    <div class="site-header__end">
      <a class="sh-nav sh-nav--ext site-header__docs" href="{{{{docs}}}}/">Docs{{{{i:external:16}}}}</a>
      <a class="sh-btn sh-btn-hero site-header__app" href="{{{{app}}}}/agents">Open the app{{{{i:arrow:16}}}}</a>
      <button class="sh-iconbtn site-header__menu" type="button" aria-label="Open menu" aria-expanded="false" aria-controls="menu" hidden>{{{{i:menu:24}}}}</button>
    </div>
  </div>
</header>"#,
        authors = aria(current == Current::Authors),
    )
}

/// The phone and tablet menu: a full-height sheet on the navy ground, its top
/// bar exactly where the page header's is, so nothing jumps. A modal dialog:
/// focus moves in and is trapped, Esc and Close return it to "Open menu".
pub(crate) const MENU: &str = r#"<div class="menu sh-hero sh-sheet-up" id="menu" role="dialog" aria-modal="true" aria-label="Menu" hidden>
  <svg class="hexgrid hexgrid--faint" aria-hidden="true" focusable="false"><defs><pattern id="hexgrid-menu" width="56" height="97" patternUnits="userSpaceOnUse"><path d="M28 0 L56 16.2 L56 48.5 L28 64.7 L0 48.5 L0 16.2 Z M28 64.7 L28 97"/></pattern></defs><rect width="100%" height="100%" fill="url(#hexgrid-menu)"/></svg>
  <div class="wrap menu__bar">
    <a class="site-header__home" href="/" aria-label="Shonin home"><img src="{{asset:lockup-reversed}}" alt="Shonin" width="74" height="24"></a>
    <button class="sh-iconbtn menu__close" type="button" aria-label="Close menu">{{i:close:24}}</button>
  </div>
  <nav class="wrap menu__nav" aria-label="Main">
    <ul>
      <li class="sh-reveal dl-80"><a class="menu__link" href="/#how"><span class="menu__text"><span class="menu__title">How it works</span><span class="menu__sub">Attested machines and trade-only keys</span></span><span class="menu__go">{{i:chev-right:24}}</span></a></li>
      <li class="sh-reveal dl-130"><a class="menu__link" href="/#traders"><span class="menu__text"><span class="menu__title">For Traders</span><span class="menu__sub">Know what an agent will do before you back it</span></span><span class="menu__go">{{i:chev-right:24}}</span></a></li>
      <li class="sh-reveal dl-180"><a class="menu__link" href="/authors"><span class="menu__text"><span class="menu__title">For Authors</span><span class="menu__sub">Publish an agent for 1 USDC</span></span><span class="menu__go">{{i:chev-right:24}}</span></a></li>
      <li class="sh-reveal dl-230"><a class="menu__link" href="/#fees"><span class="menu__text"><span class="menu__title">Fees</span><span class="menu__sub">Paid on new profit, never twice for the same gain</span></span><span class="menu__go">{{i:chev-right:24}}</span></a></li>
      <li class="sh-reveal dl-280"><a class="menu__link" href="{{docs}}/"><span class="menu__text"><span class="menu__title">Docs</span><span class="menu__sub sh-mono">docs.shonin.fi</span></span><span class="menu__go">{{i:external:22}}<span class="vh">(opens the docs site)</span></span></a></li>
    </ul>
  </nav>
  <div class="wrap menu__end sh-reveal dl-360">
    <p class="risk-line">{{i:concern:18}}<span>Agents can lose money. Nothing here stops that.</span></p>
    <div class="menu__actions">
      <a class="sh-btn sh-btn-hero sh-btn-lg" href="{{app}}/agents">Open the app{{i:arrow:18}}</a>
      <a class="sh-btn sh-btn-hero-2 sh-btn-lg" href="{{app}}/publish">Publish an agent</a>
    </div>
    <div class="menu__brand">
      <img src="{{asset:tagline-reversed}}" alt="Shonin, verifiable autonomous finance" width="124" height="40">
      <p>Attested agents.<br>Explicit constraints.</p>
    </div>
  </div>
</div>"#;

/// The footer band: brand, three link columns, the legal and social row, and
/// the risk line.
const FOOTER: &str = r#"<footer class="site-footer sh-hero">
  <div class="wrap">
    <div class="site-footer__cols">
      <div class="site-footer__brand">
        <img src="{{asset:tagline-reversed}}" alt="Shonin, verifiable autonomous finance" width="173" height="56">
        <p>Attested agents. Explicit constraints.</p>
      </div>
      <nav class="site-footer__col" aria-label="Product">
        <p class="sh-label">Product</p>
        <ul>
          <li><a href="{{app}}/agents">Agents</a></li>
          <li><a href="{{app}}/portfolio">Portfolio</a></li>
          <li><a href="{{app}}/publish">Publish an agent</a></li>
          <li><a href="{{app}}/status">Status</a></li>
        </ul>
      </nav>
      <nav class="site-footer__col" aria-label="Learn">
        <p class="sh-label">Learn</p>
        <ul>
          <li><a href="{{docs}}/basics/trading-agents/">How Shonin works</a></li>
          <li><a href="{{docs}}/verify/">Verification</a></li>
          <li><a href="{{docs}}/fees/">Fees</a></li>
          <li><a href="{{docs}}/tank/">The tank</a></li>
          <li><a href="{{docs}}/reference/glossary/">Glossary</a></li>
        </ul>
      </nav>
      <nav class="site-footer__col" aria-label="Shonin">
        <p class="sh-label">Shonin</p>
        <ul>
          <li><a href="{{docs}}/">Docs</a></li>
          <li><a href="https://github.com/ShoninFi">Source code</a></li>
        </ul>
      </nav>
    </div>
    <div class="site-footer__end">
      <div class="site-footer__row">
        <nav class="site-footer__legal" aria-label="Legal">
          <span>© 2026 Shonin</span>
          <a href="/terms">Terms of Use</a>
          <a href="/privacy">Privacy Policy</a>
        </nav>
        <ul class="site-footer__social" aria-label="Shonin elsewhere">
          {{social}}
        </ul>
      </div>
      <p class="site-footer__risk">Agents can lose money. A review isn't a prediction, and a track record isn't a promise. These pages describe the platform as designed.</p>
    </div>
  </div>
</footer>"#;

/// The 404's footer: only the legal and social row and the risk line, on the
/// page's own navy ground.
const FOOTER_COMPACT: &str = r#"<footer class="site-footer site-footer--compact sh-hero">
  <div class="wrap">
    <div class="site-footer__end">
      <div class="site-footer__row">
        <nav class="site-footer__legal" aria-label="Legal">
          <span>© 2026 Shonin</span>
          <a href="/terms">Terms of Use</a>
          <a href="/privacy">Privacy Policy</a>
        </nav>
        <ul class="site-footer__social" aria-label="Shonin elsewhere">
          {{social}}
        </ul>
      </div>
      <p class="site-footer__risk">Agents can lose money. A review isn't a prediction, and a track record isn't a promise. These pages describe the platform as designed.</p>
    </div>
  </div>
</footer>"#;
