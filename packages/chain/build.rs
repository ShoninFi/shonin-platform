//! Read every contract interface into one set of bindings.
//!
//! `sol!` reads one file and can't follow a Solidity `import`, but the
//! interfaces share types (`Allowances` is used by the prices list, the
//! factory and the vault). So this joins every file under
//! `contracts/evm/src/interfaces`, without their pragmas and imports, into one
//! `sol!` block, and each contract's module re-exports it. A type is still
//! written once, in the interface that owns it.
//!
//! With `solana-ix`, it also writes the Solana programs' ids, decided as each
//! program decides its own (see `shonin-program-id`), so the instructions this
//! crate builds name the programs this build's network deployed.

use std::fs;
use std::path::Path;

fn main() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contracts/evm/src/interfaces");
    println!("cargo:rerun-if-changed={}", dir.display());

    let mut files: Vec<_> = fs::read_dir(&dir)
        .expect("the contracts' interfaces are beside this crate")
        .map(|entry| entry.expect("a readable entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "sol"))
        .collect();
    files.sort();

    let mut solidity = String::new();
    for file in files {
        println!("cargo:rerun-if-changed={}", file.display());
        let text = fs::read_to_string(&file).expect("a readable interface");
        let mut in_import = false;
        for line in text.lines() {
            let trimmed = line.trim_start();
            if in_import || trimmed.starts_with("import ") {
                // An import may run over several lines; it ends at its `;`.
                in_import = !trimmed.ends_with(';');
                continue;
            }
            if trimmed.starts_with("pragma ") || trimmed.starts_with("// SPDX") {
                continue;
            }
            solidity.push_str(line);
            solidity.push('\n');
        }
    }

    let out =
        Path::new(&std::env::var("OUT_DIR").expect("cargo sets OUT_DIR")).join("interfaces.rs");
    fs::write(out, format!("alloy_sol_types::sol! {{\n{solidity}\n}}\n"))
        .expect("the bindings are written");

    if std::env::var_os("CARGO_FEATURE_SOLANA_IX").is_some() {
        solana_program_ids();
    }
}

/// `solana_program_ids.rs`: one `Pubkey` constant per program the platform
/// calls, under the same network features the programs' crates take. The
/// stand-in venue and driver are never built for mainnet, so there they keep
/// their local ids, as `Cargo.toml`'s `mainnet` feature leaves them.
fn solana_program_ids() {
    let mainnet = std::env::var_os("CARGO_FEATURE_MAINNET").is_some();
    let devnet = std::env::var_os("CARGO_FEATURE_DEVNET").is_some();
    let programs = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contracts/solana/programs");
    let mut rust = String::new();
    for (constant, lib, deployed_on_mainnet) in [
        ("REGISTRY", "shonin_registry", true),
        ("PUBLISHING", "shonin_publishing", true),
        ("PRICES", "shonin_prices", true),
        ("STATION", "shonin_station", true),
        ("VAULT", "shonin_vault", true),
        ("LOCAL_VENUE", "shonin_local_venue", false),
        ("LOCAL_DRIVER", "shonin_local_driver", false),
    ] {
        let dir = programs.join(lib.replace('_', "-"));
        let id = shonin_program_id::program_id(lib, &dir, mainnet && deployed_on_mainnet, devnet);
        rust.push_str(&format!(
            "pub const {constant}: Pubkey = solana_pubkey::pubkey!(\"{id}\");\n"
        ));
    }
    let out = Path::new(&std::env::var("OUT_DIR").expect("cargo sets OUT_DIR"))
        .join("solana_program_ids.rs");
    fs::write(out, rust).expect("the program ids are written");
}
