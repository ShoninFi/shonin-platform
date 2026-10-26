//! The keeper's keys in the mainnet deployment files.
//!
//! A mainnet deploy records the keeper as nobody (the zero address, or the
//! system program's key on Solana): its hot keys are made later, by the
//! owner. `mainnet-record-keeper` writes the keeper's address into each
//! deployment file once they exist, changing nothing else in the file and
//! sending nothing; the read-back (`mainnet-verify`, `mainnet-solana-verify`)
//! then checks what is recorded. The services read the keeper from the file,
//! so the switch is a file, not a build.

use std::path::Path;

use alloy::primitives::Address;
use anyhow::{Context, Result, anyhow, bail};
use chain::solana::Pubkey;
use serde_json::Value;

use crate::mainnet_guard::{self, Mode};

/// What recording the keeper came to for one file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Recorded {
    /// The file already names this keeper: nothing to write.
    Already,
    /// The file's new text, naming the keeper.
    Changed(String),
}

fn roles_of(value: &mut Value) -> Result<&mut serde_json::Map<String, Value>> {
    value
        .get_mut("roles")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| anyhow!("the deployment file records no roles"))
}

/// A role as the file records it: "" when it is missing (or null), and
/// refused when it is anything but a string (an object, an array, a number,
/// a boolean). Read as nobody, a keeper in another form would be replaced
/// without REPLACE, and a deployer or platform in another form could never
/// be told apart from the keeper.
fn recorded(value: Option<&Value>, what: &str) -> Result<String> {
    match value {
        None | Some(Value::Null) => Ok(String::new()),
        Some(Value::String(text)) => Ok(text.clone()),
        Some(other) => bail!(
            "the deployment file records {what} as {other}, which is not an address: correct the \
             file first. Nothing was written."
        ),
    }
}

fn role(roles: &serde_json::Map<String, Value>, name: &str) -> Result<String> {
    recorded(roles.get(name), &format!("the {name}"))
}

/// Record `keeper` as the keeper of an EVM chain's mainnet deployment file
/// (`text`), held to `admin`. Refused: the zero address, the admin, the
/// platform's fee address or the deployer (the keeper is a hot key of its
/// own, holding no other role), a file whose admin isn't ADMIN, and another
/// keeper already recorded unless `replace`.
pub fn record_evm(text: &str, keeper: Address, admin: Address, replace: bool) -> Result<Recorded> {
    let mut value: Value = serde_json::from_str(text).context("reading the deployment file")?;
    let record = value
        .get(crate::mainnet_evm::MAINNET_KEY)
        .ok_or_else(|| anyhow!("the deployment file has no mainnet record"))?;
    let deployer = recorded(record.get("deployer"), "the deployer")?;
    let roles = roles_of(&mut value)?;
    // A role recorded in a form that isn't an address is refused, never read
    // as nobody: read as nobody, a keeper would be replaced without REPLACE,
    // and a deployer could never be told apart from the keeper.
    let parse = |text: String, what: &str| {
        text.trim().parse::<Address>().map_err(|_| {
            anyhow!(
                "the deployment file records {what} as {text:?}, which is not an EVM address: \
                 correct the file first. Nothing was written."
            )
        })
    };
    let deployer = parse(deployer, "the deployer")?;
    let platform = parse(role(roles, "platform")?, "the platform's fee address")?;
    let keeper_text = role(roles, "keeper")?;
    let current = if keeper_text.is_empty() {
        Address::ZERO
    } else {
        parse(keeper_text, "the keeper")?
    };
    let recorded_admin = parse(role(roles, "admin")?, "the admin")?;
    if recorded_admin != admin {
        bail!(
            "the deployment file's admin is {recorded_admin}, not ADMIN={admin}. Nothing was written."
        );
    }
    if keeper == Address::ZERO {
        bail!("the keeper can't be the zero address. Nothing was written.");
    }
    for (name, other) in [
        ("the admin", admin),
        ("the platform's fee address", platform),
        ("the deployer", deployer),
    ] {
        if keeper == other {
            bail!(
                "the keeper ({keeper}) is {name}: the keeper is a hot key holding no other role. \
                 Nothing was written."
            );
        }
    }
    if current == keeper {
        return Ok(Recorded::Already);
    }
    if current != Address::ZERO && !replace {
        bail!(
            "the deployment file already records the keeper {current}: pass REPLACE=1 to record \
             {keeper} in its place. Nothing was written."
        );
    }
    roles.insert("keeper".to_string(), Value::String(keeper.to_string()));
    Ok(Recorded::Changed(
        serde_json::to_string_pretty(&value)? + "\n",
    ))
}

/// Record `keeper` as the keeper of Solana's mainnet deployment file, held to
/// `admin`, on the same rules as [`record_evm`]; and the key must be one a
/// key can sign for (on the ed25519 curve), since the keeper signs and pays.
pub fn record_solana(text: &str, keeper: Pubkey, admin: Pubkey, replace: bool) -> Result<Recorded> {
    let mut value: Value = serde_json::from_str(text).context("reading the deployment file")?;
    let roles = roles_of(&mut value)?;
    // Every role is a Solana address, refused otherwise (as on EVM): an
    // unreadable keeper is never taken for nobody.
    for name in ["admin", "platform", "deployer", "keeper"] {
        let text = role(roles, name)?;
        if !text.is_empty() && text.parse::<Pubkey>().is_err() {
            bail!(
                "the deployment file records the {name} as {text:?}, which is not a Solana \
                 address: correct the file first. Nothing was written."
            );
        }
    }
    let recorded_admin = role(roles, "admin")?;
    if recorded_admin != admin.to_string() {
        bail!(
            "the deployment file's admin is {recorded_admin}, not ADMIN={admin}. Nothing was written."
        );
    }
    if keeper == Pubkey::default() || !keeper.is_on_curve() {
        bail!(
            "the keeper ({keeper}) is not a key anything can sign for: the keeper signs and pays \
             its own transactions. Nothing was written."
        );
    }
    for name in ["admin", "platform", "deployer"] {
        if role(roles, name)? == keeper.to_string() {
            bail!(
                "the keeper ({keeper}) is the {name}: the keeper is a hot key holding no other \
                 role. Nothing was written."
            );
        }
    }
    let current = role(roles, "keeper")?;
    if current == keeper.to_string() {
        return Ok(Recorded::Already);
    }
    if !current.is_empty() && current != Pubkey::default().to_string() && !replace {
        bail!(
            "the deployment file already records the keeper {current}: pass REPLACE=1 to record \
             {keeper} in its place. Nothing was written."
        );
    }
    roles.insert("keeper".to_string(), Value::String(keeper.to_string()));
    Ok(Recorded::Changed(
        serde_json::to_string_pretty(&value)? + "\n",
    ))
}

/// Where each mode reads and writes the deployment files: a fork's under
/// `.shonin/mainnet-fork` alone; otherwise this repository's committed
/// `config/deployments/mainnet` alone.
pub fn check_dir(mode: Mode, dir: &Path) -> Result<std::path::PathBuf> {
    match mode {
        Mode::Fork if mainnet_guard::is_committed_dir(dir) => bail!(
            "a fork rehearsal records the keeper in the fork's deployment files, never in \
             config/deployments/mainnet ({}). Nothing was written.",
            dir.display()
        ),
        Mode::Fork => mainnet_guard::resolved(dir),
        _ => mainnet_guard::deployments_dir(Mode::Send, dir),
    }
}

/// `mainnet-record-keeper`: for each file (path, chain kind), print what the
/// keeper's recording changes, and with `write` write it.
pub fn run(
    files: &[(std::path::PathBuf, bool)],
    mode: Mode,
    dir: &Path,
    keeper: &str,
    admin: &str,
    replace: bool,
    write: bool,
) -> Result<()> {
    if mode == Mode::Send {
        bail!(
            "recording the keeper sends nothing: leave DEPLOYER_KEYFILE and CONFIRM_MAINNET out \
             (WRITE=1 writes the files)."
        );
    }
    // The directory as checked, resolved, and each file in it by name alone:
    // never a path the check didn't see, and never a file that is a link to
    // another (a fork's file linked to a committed one would write it).
    let dir = check_dir(mode, dir)?;
    for (given, solana) in files {
        let name = given
            .file_name()
            .ok_or_else(|| anyhow!("{} names no file", given.display()))?;
        let file = &dir.join(name);
        mainnet_guard::refuse_linked_file(file)?;
        let text = std::fs::read_to_string(file)
            .with_context(|| format!("reading {}: deploy first", file.display()))?;
        let recorded = if *solana {
            let parse = |text: &str, what: &str| {
                text.trim()
                    .parse::<Pubkey>()
                    .map_err(|_| anyhow!("{what} is not a Solana address ({text})"))
            };
            record_solana(
                &text,
                parse(keeper, "KEEPER")?,
                parse(admin, "ADMIN")?,
                replace,
            )?
        } else {
            let parse = |text: &str, what: &str| {
                text.trim()
                    .parse::<Address>()
                    .map_err(|_| anyhow!("{what} is not an EVM address ({text})"))
            };
            record_evm(
                &text,
                parse(keeper, "KEEPER")?,
                parse(admin, "ADMIN")?,
                replace,
            )?
        };
        match recorded {
            Recorded::Already => println!("{}: the keeper is {keeper} already", file.display()),
            Recorded::Changed(new) if write => {
                mainnet_guard::write_file(file, new)?;
                println!("{}: the keeper is now {keeper}", file.display());
            }
            Recorded::Changed(_) => println!(
                "{}: would record the keeper {keeper} (WRITE=1 writes it). Nothing was written.",
                file.display()
            ),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chain::solana::{Keypair, Signer};

    fn evm_file(admin: Address, deployer: Address, keeper: Address) -> String {
        serde_json::json!({
            "chain_key": "evm:8453",
            "roles": {"admin": admin.to_string(), "platform": admin.to_string(),
                      "keeper": keeper.to_string(), "executor": Address::ZERO.to_string()},
            "mainnet": {"deployer": deployer.to_string()}
        })
        .to_string()
    }

    #[test]
    fn the_keeper_is_recorded_in_the_evm_file_and_holds_no_other_role() {
        let admin = Address::repeat_byte(1);
        let deployer = Address::repeat_byte(2);
        let keeper = Address::repeat_byte(3);
        let text = evm_file(admin, deployer, Address::ZERO);
        let Recorded::Changed(new) = record_evm(&text, keeper, admin, false).unwrap() else {
            panic!("nothing changed");
        };
        let value: Value = serde_json::from_str(&new).unwrap();
        assert_eq!(value["roles"]["keeper"], keeper.to_string());
        // Nothing else in the file moves.
        assert_eq!(value["roles"]["admin"], admin.to_string());
        assert_eq!(value["mainnet"]["deployer"], deployer.to_string());
        assert_eq!(
            record_evm(&new, keeper, admin, false).unwrap(),
            Recorded::Already
        );
        for refused in [Address::ZERO, admin, deployer] {
            assert!(
                record_evm(&text, refused, admin, false).is_err(),
                "{refused}"
            );
        }
        // Held to ADMIN.
        assert!(record_evm(&text, keeper, Address::repeat_byte(9), false).is_err());
        // Another keeper replaces it only when asked.
        let other = Address::repeat_byte(4);
        let error = record_evm(&new, other, admin, false)
            .unwrap_err()
            .to_string();
        assert!(error.contains("REPLACE=1"), "{error}");
        assert!(matches!(
            record_evm(&new, other, admin, true).unwrap(),
            Recorded::Changed(_)
        ));
    }

    #[test]
    fn the_keeper_is_recorded_in_the_solana_file_as_a_key_that_can_sign() {
        let admin = Keypair::new().pubkey();
        let deployer = Keypair::new().pubkey();
        let keeper = Keypair::new().pubkey();
        let text = serde_json::json!({
            "programs": {},
            "roles": {"deployer": deployer.to_string(), "admin": admin.to_string(),
                      "platform": admin.to_string(), "keeper": Pubkey::default().to_string()}
        })
        .to_string();
        let Recorded::Changed(new) = record_solana(&text, keeper, admin, false).unwrap() else {
            panic!("nothing changed");
        };
        let value: Value = serde_json::from_str(&new).unwrap();
        assert_eq!(value["roles"]["keeper"], keeper.to_string());
        let (pda, _) = Pubkey::find_program_address(&[b"x"], &Pubkey::new_unique());
        for refused in [Pubkey::default(), pda, admin, deployer] {
            assert!(
                record_solana(&text, refused, admin, false).is_err(),
                "{refused}"
            );
        }
        assert!(record_solana(&new, Keypair::new().pubkey(), admin, false).is_err());
    }

    #[test]
    fn a_role_recorded_as_anything_but_a_string_is_refused_on_both_chains() {
        let forms = [
            serde_json::json!({"address": "x"}),
            serde_json::json!(["x"]),
            serde_json::json!(7),
            serde_json::json!(false),
        ];
        let admin = Address::repeat_byte(1);
        let keeper = Address::repeat_byte(3);
        for form in &forms {
            for name in ["admin", "platform", "keeper"] {
                let mut value: Value =
                    serde_json::from_str(&evm_file(admin, Address::repeat_byte(2), Address::ZERO))
                        .unwrap();
                value["roles"][name] = form.clone();
                let error = record_evm(&value.to_string(), keeper, admin, false).unwrap_err();
                assert!(
                    error.to_string().contains("not an address"),
                    "{name} {form}: {error}"
                );
            }
            let mut value: Value =
                serde_json::from_str(&evm_file(admin, Address::repeat_byte(2), Address::ZERO))
                    .unwrap();
            value["mainnet"]["deployer"] = form.clone();
            assert!(
                record_evm(&value.to_string(), keeper, admin, false).is_err(),
                "{form}"
            );
        }
        // A keeper left out, or null, is nobody.
        for none in [None, Some(Value::Null)] {
            let mut value: Value =
                serde_json::from_str(&evm_file(admin, Address::repeat_byte(2), Address::ZERO))
                    .unwrap();
            let roles = value["roles"].as_object_mut().unwrap();
            match &none {
                None => drop(roles.remove("keeper")),
                Some(null) => drop(roles.insert("keeper".into(), null.clone())),
            }
            assert!(matches!(
                record_evm(&value.to_string(), keeper, admin, false).unwrap(),
                Recorded::Changed(_)
            ));
        }

        let admin = Keypair::new().pubkey();
        let deployer = Keypair::new().pubkey();
        let keeper = Keypair::new().pubkey();
        let solana_file = || {
            serde_json::json!({
                "programs": {},
                "roles": {"deployer": deployer.to_string(), "admin": admin.to_string(),
                          "platform": admin.to_string(), "keeper": Pubkey::default().to_string()}
            })
        };
        for form in &forms {
            for name in ["admin", "platform", "deployer", "keeper"] {
                let mut value = solana_file();
                value["roles"][name] = form.clone();
                let error = record_solana(&value.to_string(), keeper, admin, false).unwrap_err();
                assert!(
                    error.to_string().contains("not an address"),
                    "{name} {form}: {error}"
                );
            }
        }
        for none in [None, Some(Value::Null)] {
            let mut value = solana_file();
            let roles = value["roles"].as_object_mut().unwrap();
            match &none {
                None => drop(roles.remove("keeper")),
                Some(null) => drop(roles.insert("keeper".into(), null.clone())),
            }
            assert!(matches!(
                record_solana(&value.to_string(), keeper, admin, false).unwrap(),
                Recorded::Changed(_)
            ));
        }
    }

    #[test]
    fn a_fork_never_records_the_keeper_in_the_committed_files_and_nothing_is_sent() {
        let committed = mainnet_guard::committed_dir();
        assert!(check_dir(Mode::Fork, &committed).is_err());
        let elsewhere = std::env::temp_dir().join("shonin-keeper-elsewhere");
        assert!(check_dir(Mode::DryRun, &elsewhere).is_err());
        assert!(run(&[], Mode::Send, &committed, "", "", false, false).is_err());
    }
}
