//! Write everything the frontends' dists need from the design system.
//!
//! tokens --out packages/frontend/ui/dist
//!
//! Into `<out>`:
//! - `tokens.css`: the tokens as custom properties, and one `.t-*` class per
//!   type style;
//! - `fonts.css` and `fonts/*.woff2` (with the licence notice): the
//!   self-hosted variable fonts, served at `/fonts/`;
//! - `sh.css`: every atom and component stylesheet, in cascade order, and
//!   `styles/<name>.css` for each on its own;
//! - `logos/*.svg`: the logo files, served at `/logos/`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Parser;

#[derive(Parser)]
#[command(about = "Writes the design system's CSS, fonts and logos for the frontends")]
struct Args {
    /// The directory to write into.
    #[arg(long)]
    out: PathBuf,
}

fn write(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("making {}", parent.display()))?;
    }
    // Only rewrite what changed, so a watching dev server doesn't rebuild.
    if std::fs::read(path).is_ok_and(|old| old == bytes) {
        return Ok(());
    }
    std::fs::write(path, bytes).with_context(|| format!("writing {}", path.display()))
}

fn main() -> Result<()> {
    let args = Args::parse();
    let tokens = ui::Tokens::parse(ui::TOKENS).context("tokens.json")?;
    let out = &args.out;

    write(&out.join("tokens.css"), tokens.css().as_bytes())?;
    write(&out.join("fonts.css"), tokens.fonts_css().as_bytes())?;
    for (name, bytes) in ui::FONTS {
        write(&out.join("fonts").join(name), bytes)?;
    }
    write(&out.join("fonts/README"), ui::FONTS_README.as_bytes())?;
    write(&out.join("sh.css"), ui::styles::bundle().as_bytes())?;
    for (name, css) in ui::styles::ALL {
        write(
            &out.join("styles").join(format!("{name}.css")),
            css.as_bytes(),
        )?;
    }
    for (name, svg) in ui::logos::FILES {
        write(&out.join("logos").join(name), svg.as_bytes())?;
    }
    Ok(())
}
