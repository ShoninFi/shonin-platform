//! Builds the marketing site, or builds it and serves it locally.
//!
//! `build` writes the site; `serve` builds it and serves it the way Workers
//! static assets do: `/x` from `x.html`, `/` from `index.html`, and anything
//! else that isn't a file from `404.html` with a 404 status.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderValue, Request, Response, StatusCode, header};
use clap::{Parser, Subcommand};
use marketing::Links;

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
    /// Where this site itself lives, for canonical addresses.
    #[arg(long, default_value = "https://shonin.fi")]
    site_url: String,
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
            serve(site.out, port).await
        }
    }
}

fn build(site: &Site) -> Result<()> {
    let trim = |url: &str| url.trim_end_matches('/').to_string();
    let links = Links {
        app: trim(&site.app_url),
        docs: trim(&site.docs_url),
        site: trim(&site.site_url),
    };
    let files = marketing::site(&links)?;
    marketing::write(&files, &site.out)?;
    tracing::info!(out = %site.out.display(), files = files.len(), "the marketing site is written");
    Ok(())
}

async fn serve(dir: PathBuf, port: u16) -> Result<()> {
    let app = axum::Router::new().fallback(file).with_state(dir);
    let address = SocketAddr::from(([127, 0, 0, 1], port));
    let listener = tokio::net::TcpListener::bind(address)
        .await
        .with_context(|| format!("listening on {address}"))?;
    tracing::info!("the marketing site is at http://localhost:{port}/");
    axum::serve(listener, app).await?;
    Ok(())
}

/// One request, answered from the built site as the Worker would.
async fn file(State(dir): State<PathBuf>, request: Request<Body>) -> Response<Body> {
    let path = request.uri().path();
    let found = resolve(&dir, path);
    let (status, file) = match found {
        Some(file) => (StatusCode::OK, file),
        None => (StatusCode::NOT_FOUND, dir.join("404.html")),
    };
    match tokio::fs::read(&file).await {
        Ok(bytes) => {
            let mut response = Response::new(Body::from(bytes));
            *response.status_mut() = status;
            let headers = response.headers_mut();
            headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(kind(&file)));
            // The same headers the Worker sends for every path, so the
            // Content-Security-Policy is exercised locally too.
            for (name, value) in site_headers(&dir).await {
                // Plain http on localhost: nothing to upgrade.
                let value = value.replace("; upgrade-insecure-requests", "");
                if let (Ok(name), Ok(value)) = (
                    header::HeaderName::try_from(name),
                    HeaderValue::try_from(value),
                ) {
                    headers.insert(name, value);
                }
            }
            response
        }
        Err(_) => {
            let mut response = Response::new(Body::from("not found"));
            *response.status_mut() = StatusCode::NOT_FOUND;
            response
        }
    }
}

/// The file a path names, if the site has one. Never leaves `dir`.
fn resolve(dir: &Path, path: &str) -> Option<PathBuf> {
    let relative = path.trim_start_matches('/');
    if relative
        .split('/')
        .any(|part| part == ".." || part.starts_with('.'))
    {
        return None;
    }
    let candidates = if relative.is_empty() {
        vec![dir.join("index.html")]
    } else {
        vec![
            dir.join(relative),
            dir.join(format!("{}.html", relative.trim_end_matches('/'))),
        ]
    };
    candidates
        .into_iter()
        .find(|candidate| candidate.is_file() && !candidate.ends_with("404.html"))
}

/// The `/*` block of the built `_headers` file.
async fn site_headers(dir: &Path) -> Vec<(String, String)> {
    let text = tokio::fs::read_to_string(dir.join("_headers"))
        .await
        .unwrap_or_default();
    let mut in_all = false;
    let mut out = Vec::new();
    for line in text.lines() {
        if !line.starts_with(' ') {
            in_all = line.trim() == "/*";
            continue;
        }
        if let (true, Some((name, value))) = (in_all, line.trim().split_once(':')) {
            out.push((name.trim().to_string(), value.trim().to_string()));
        }
    }
    out
}

fn kind(file: &Path) -> &'static str {
    match file.extension().and_then(|ext| ext.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("xml") => "application/xml; charset=utf-8",
        Some("woff2") => "font/woff2",
        Some("txt") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}
