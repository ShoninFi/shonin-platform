//! Read every contract interface into one set of bindings.
//!
//! `sol!` reads one file and can't follow a Solidity `import`, but the
//! interfaces share types (`Allowances` is used by the prices list, the
//! factory and the vault). So this joins every file under
//! `contracts/evm/src/interfaces`, without their pragmas and imports, into one
//! `sol!` block, and each contract's module re-exports it. A type is still
//! written once, in the interface that owns it.

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
}
