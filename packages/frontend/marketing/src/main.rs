//! The marketing site: shonin.fi, once it is real.
//!
//! Static pages, generated from Rust with the same small HTML helpers the
//! app's pre-render uses: no wasm, no wallet, no data. `build` writes the
//! site; `serve` builds it and serves it for local development.
//!
//! This is the scaffold: the design's shell and sections, with placeholder
//! copy in the docs' own words and no illustrations yet.

mod pages;

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(about = "Builds and serves the marketing site")]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(clap::Args, Clone)]
struct Site {
    /// Where to write the site.
    #[arg(long)]
    out: PathBuf,
    /// Where "Open the app" and the app's pages are.
    #[arg(long, default_value = "https://app.shonin.fi")]
    app_url: String,
    /// Where the documentation is.
    #[arg(long, default_value = "https://docs.shonin.fi")]
    docs_url: String,
}

#[derive(Subcommand)]
enum Command {
    /// Write the site.
    Build(Site),
    /// Write the site, then serve it.
    Serve {
        #[command(flatten)]
        site: Site,
        #[arg(long, default_value_t = 19_082)]
        port: u16,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_target(false)
        .init();

    match Args::parse().command {
        Command::Build(site) => build(&site),
        Command::Serve { site, port } => {
            build(&site)?;
            serve(&site.out, port).await
        }
    }
}

fn build(site: &Site) -> Result<()> {
    let links = pages::Links {
        app: site.app_url.trim_end_matches('/').to_string(),
        docs: site.docs_url.trim_end_matches('/').to_string(),
    };
    let tokens = ui::Tokens::parse(ui::TOKENS).context("the design tokens")?;

    let _ = std::fs::remove_dir_all(&site.out);
    write(&site.out.join("index.html"), &pages::home(&links))?;
    write(
        &site.out.join("authors/index.html"),
        &pages::authors(&links),
    )?;
    write(&site.out.join("404.html"), &pages::not_found(&links))?;
    write(&site.out.join("styles/tokens.css"), &tokens.css())?;
    write(&site.out.join("styles/marketing.css"), pages::STYLE)?;
    write(&site.out.join("_headers"), HEADERS)?;
    write(&site.out.join("robots.txt"), ROBOTS)?;
    tracing::info!(out = %site.out.display(), "the marketing site is written");
    Ok(())
}

fn write(path: &Path, contents: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("making {}", parent.display()))?;
    }
    std::fs::write(path, contents).with_context(|| format!("writing {}", path.display()))
}

async fn serve(dir: &Path, port: u16) -> Result<()> {
    let files = tower_http::services::ServeDir::new(dir)
        .not_found_service(tower_http::services::ServeFile::new(dir.join("404.html")));
    let app = axum::Router::new().fallback_service(files);
    let address = SocketAddr::from(([127, 0, 0, 1], port));
    let listener = tokio::net::TcpListener::bind(address)
        .await
        .with_context(|| format!("listening on {address}"))?;
    tracing::info!("the marketing site is at http://localhost:{port}/");
    axum::serve(listener, app).await?;
    Ok(())
}

/// Workers static assets read these from the output root, as the app's do.
const HEADERS: &str = "/*
  X-Content-Type-Options: nosniff
  X-Frame-Options: DENY
  Referrer-Policy: strict-origin-when-cross-origin
  Permissions-Policy: camera=(), microphone=(), geolocation=()
";

/// A scaffold is not for indexing. When this site takes shonin.fi, this and
/// the pages' robots meta go.
const ROBOTS: &str = "User-agent: *\nDisallow: /\n";
