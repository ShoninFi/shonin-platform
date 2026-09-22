//! Publishing the reference strategies, through the platform's own flows.
//!
//! Nothing here is a shortcut. The strategies are packed, submitted, built,
//! registered and reviewed exactly as an author's would be, so what the site
//! shows afterwards is what the platform did rather than what a fixture said.

use std::path::Path;

use anyhow::Result;
use chain::{Client, Deployments};

pub async fn run(
    _client: &Client,
    _deployments: &Deployments,
    _api: &str,
    _bots: Option<&Path>,
    _store: &Path,
) -> Result<()> {
    anyhow::bail!("seeding arrives with the build service")
}
