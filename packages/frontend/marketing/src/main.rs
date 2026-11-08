//! Builds the marketing site, or builds it and serves it locally; or builds
//! the shutdown notice app.shonin.fi serves.
//!
//! `build` writes the site; `serve` builds it and serves it the way Workers
//! static assets do: `/x` from `x.html`, `/` from `index.html`, and anything
//! else that isn't a file from `404.html` with a 404 status.
//!
//! `shutdown` writes the notice; with `--port` it serves it too, as its
//! Worker does (`not_found_handling: "single-page-application"`): anything
//! that isn't a file is `index.html`, with a 200 status.

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

#[derive(clap::Args, Clone)]
struct Notice {
    /// Where to write the notice.
    #[arg(long)]
    out: PathBuf,
    /// Where the notice is served (the app's old origin).
    #[arg(long, default_value = "https://app.shonin.fi")]
    app_url: String,
    /// Where the documentation is.
    #[arg(long, default_value = "https://docs.shonin.fi")]
    docs_url: String,
    /// Where the marketing site is.
    #[arg(long, default_value = "https://shonin.fi")]
    site_url: String,
    /// Serve the notice on this port once it is written.
    #[arg(long)]
    port: Option<u16>,
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
    /// Write the shutdown notice app.shonin.fi serves (and serve it, with --port).
    Shutdown(Notice),
}

/// What the server answers for a path that isn't a file.
#[derive(Clone, Copy)]
enum Missing {
    /// `404.html`, with a 404 status: the marketing site.
    NotFound,
    /// `index.html`, with a 200 status: the shutdown notice.
    Index,
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
            serve(site.out, port, Missing::NotFound).await
        }
        Command::Shutdown(notice) => {
            shutdown(&notice)?;
            match notice.port {
                Some(port) => serve(notice.out, port, Missing::Index).await,
                None => Ok(()),
            }
        }
    }
}

fn trim(url: &str) -> String {
    url.trim_end_matches('/').to_string()
}

fn build(site: &Site) -> Result<()> {
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

fn shutdown(notice: &Notice) -> Result<()> {
    let links = Links {
        app: trim(&notice.app_url),
        docs: trim(&notice.docs_url),
        site: trim(&notice.site_url),
    };
    let files = marketing::shutdown(&links)?;
    marketing::write(&files, &notice.out)?;
    tracing::info!(out = %notice.out.display(), files = files.len(), "the shutdown notice is written");
    Ok(())
}

async fn serve(dir: PathBuf, port: u16, missing: Missing) -> Result<()> {
    let app = axum::Router::new()
        .fallback(file)
        .with_state((dir.clone(), missing));
    let address = SocketAddr::from(([127, 0, 0, 1], port));
    let listener = tokio::net::TcpListener::bind(address)
        .await
        .with_context(|| format!("listening on {address}"))?;
    tracing::info!("serving {} at http://localhost:{port}/", dir.display());
    axum::serve(listener, app).await?;
    Ok(())
}

/// One request, answered from the built site as the Worker would.
async fn file(
    State((dir, missing)): State<(PathBuf, Missing)>,
    request: Request<Body>,
) -> Response<Body> {
    let (status, file) = answer(&dir, request.uri().path(), missing);
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

/// The status and the file the Worker answers a path with: the file the
/// path names, or for a path that names none, the 404 page (the marketing
/// site) or the notice itself with a 200 (the shutdown notice).
fn answer(dir: &Path, path: &str, missing: Missing) -> (StatusCode, PathBuf) {
    match (resolve(dir, path), missing) {
        (Some(file), _) => (StatusCode::OK, file),
        (None, Missing::NotFound) => (StatusCode::NOT_FOUND, dir.join("404.html")),
        (None, Missing::Index) => (StatusCode::OK, dir.join("index.html")),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A site holding just the two pages, in a directory of the test's own.
    fn site_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("marketing-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for file in ["index.html", "404.html"] {
            std::fs::write(dir.join(file), file).unwrap();
        }
        dir
    }

    #[test]
    fn an_old_app_address_gets_the_notice_with_a_200() {
        let dir = site_dir("notice");
        for path in [
            "/portfolio/claims/123",
            "/publish/jobs/7",
            "/you/agents/x",
            "/agents",
        ] {
            assert_eq!(
                answer(&dir, path, Missing::Index),
                (StatusCode::OK, dir.join("index.html")),
                "{path}"
            );
        }
    }

    #[test]
    fn the_marketing_site_still_answers_an_unknown_path_with_its_404() {
        let dir = site_dir("site");
        assert_eq!(
            answer(&dir, "/portfolio/claims/123", Missing::NotFound),
            (StatusCode::NOT_FOUND, dir.join("404.html"))
        );
        assert_eq!(
            answer(&dir, "/", Missing::NotFound),
            (StatusCode::OK, dir.join("index.html"))
        );
    }
}
