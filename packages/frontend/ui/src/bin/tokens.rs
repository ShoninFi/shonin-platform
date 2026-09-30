//! Write the design tokens as CSS custom properties.
//!
//! tokens --out packages/frontend/ui/dist/tokens.css

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;

#[derive(Parser)]
#[command(about = "Writes tokens.css from the design's tokens.json")]
struct Args {
    /// Where to write the stylesheet.
    #[arg(long)]
    out: PathBuf,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let tokens = ui::Tokens::parse(ui::TOKENS).context("tokens.json")?;
    if let Some(parent) = args.out.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("making {}", parent.display()))?;
    }
    std::fs::write(&args.out, tokens.css())
        .with_context(|| format!("writing {}", args.out.display()))?;
    Ok(())
}
