//! The admin's key, held in a plain key file, and the checks it passes
//! before anything is read from a chain.
//!
//! The admin is one key on every EVM chain (one `0x…` line, like the
//! deployer's file) and one ed25519 keypair on Solana (a keypair JSON). A
//! mainnet command signs its admin steps with it in this process alone:
//! the key is never handed to a child process (neither in its arguments
//! nor in its environment), never printed, logged or put in an error.
//!
//! - On mainnet (`ADMIN_KEYFILE` with `CONFIRM_MAINNET`) the file is a
//!   regular file of its own (no symlink, no other name on disk), readable
//!   by its owner alone, outside this repository and outside any `.shonin`
//!   directory; its key is `ADMIN`'s, not the deployer's, and neither a
//!   key of the public development mnemonic (on EVM and on Solana) nor a
//!   throwaway a fork rehearsal made.
//! - On a fork rehearsal the admin is a throwaway this tool makes and
//!   records under `.shonin/mainnet-fork` (or, on EVM, one of anvil's
//!   development keys): a real key's transactions, signed for a mainnet
//!   chain id, would be valid on mainnet.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use alloy::primitives::Address;
use alloy::signers::local::PrivateKeySigner;
use anyhow::{Result, anyhow, bail};
use chain::solana::{Keypair, Pubkey, Signer};

use crate::mainnet_guard::{self, Mode};

/// This repository's root.
pub fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

/// The EVM throwaway admin a fork rehearsal signs with: one `0x…` line,
/// readable by its owner alone.
pub fn evm_throwaway_file(fork_root: &Path) -> PathBuf {
    fork_root.join("evm").join("throwaway-admin")
}

/// Every EVM throwaway admin address this tool has made, one a line.
pub fn evm_throwaway_record(fork_root: &Path) -> PathBuf {
    fork_root.join("evm").join("throwaway-admin-addresses")
}

/// The Solana throwaway admin a fork rehearsal signs with (a keypair JSON).
pub fn solana_throwaway_file(fork_root: &Path) -> PathBuf {
    fork_root.join("solana").join("throwaway-admin.json")
}

/// Every Solana throwaway admin this tool has made, one public key a line.
pub fn solana_throwaway_record(fork_root: &Path) -> PathBuf {
    fork_root.join("solana").join("throwaway-admin-ids")
}

fn recorded(record: &Path) -> BTreeSet<String> {
    std::fs::read_to_string(record)
        .unwrap_or_default()
        .lines()
        .map(|line| line.trim().to_ascii_lowercase())
        .filter(|line| !line.is_empty())
        .collect()
}

fn record(record: &Path, entry: &str) -> Result<()> {
    use std::io::Write;
    if let Some(parent) = record.parent() {
        std::fs::create_dir_all(parent)?;
    }
    mainnet_guard::refuse_linked_file(record)?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(record)
        .map_err(|error| anyhow!("writing {}: {}", record.display(), error.kind()))?;
    writeln!(file, "{entry}")?;
    Ok(())
}

/// Write a new file readable by its owner alone; never over one there.
fn write_private_new(file: &Path, text: &str) -> Result<()> {
    use std::io::Write;
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut out = options
        .open(file)
        .map_err(|error| anyhow!("making {}: {}", file.display(), error.kind()))?;
    out.write_all(text.as_bytes())?;
    out.sync_all()?;
    Ok(())
}

/// The key file's own checks, before it is read: it exists, is a regular
/// file with no other name on disk, and no one but its owner may read it.
pub fn check_file(path: &Path) -> Result<()> {
    let meta = std::fs::symlink_metadata(path).map_err(|error| {
        anyhow!(
            "ADMIN_KEYFILE {} can't be read: {}. Nothing was sent.",
            path.display(),
            error.kind()
        )
    })?;
    mainnet_guard::refuse_linked_file(path)
        .map_err(|error| anyhow!("ADMIN_KEYFILE: {error} Nothing was sent."))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = meta.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            bail!(
                "ADMIN_KEYFILE {} is readable by others (mode {mode:o}): an admin key file is \
                 its owner's alone (chmod 600). Nothing was sent.",
                path.display()
            );
        }
    }
    #[cfg(not(unix))]
    let _ = meta;
    Ok(())
}

/// Where a mainnet admin key file may be: outside this repository and
/// every worktree of it, and outside any `.shonin` directory (where the tool's own files, a fork's
/// throwaways among them, are kept).
pub fn check_mainnet_place(path: &Path) -> Result<()> {
    if mainnet_guard::is_under(path, &repository()) || is_in_a_worktree(path) {
        bail!(
            "ADMIN_KEYFILE {} is inside this repository (or a worktree of it): the admin's key \
             is kept outside it, where nothing the repository holds or builds can reach it. \
             Nothing was sent.",
            path.display()
        );
    }
    let resolved = mainnet_guard::resolved(path)?;
    if resolved.components().any(|part| {
        part.as_os_str()
            .to_string_lossy()
            .eq_ignore_ascii_case(".shonin")
    }) {
        bail!(
            "ADMIN_KEYFILE {} is under a .shonin directory, where the tool's own files and a \
             fork's throwaway keys are kept. Nothing was sent.",
            path.display()
        );
    }
    Ok(())
}

/// Read an EVM key file: one `0x` hex line. Neither the key nor the text
/// ever appears in an error.
fn read_evm(path: &Path) -> Result<PrivateKeySigner> {
    let text = std::fs::read_to_string(path).map_err(|error| {
        anyhow!(
            "reading ADMIN_KEYFILE {}: {}. Nothing was sent.",
            path.display(),
            error.kind()
        )
    })?;
    text.trim().parse::<PrivateKeySigner>().map_err(|_| {
        anyhow!(
            "ADMIN_KEYFILE {} does not hold one 0x hex private key. Nothing was sent.",
            path.display()
        )
    })
}

/// Read a Solana keypair file. Neither the key nor the text ever appears
/// in an error.
fn read_solana(path: &Path) -> Result<Keypair> {
    chain::solana::keys::read_keypair(path).map_err(|_| {
        anyhow!(
            "ADMIN_KEYFILE {} is not a Solana keypair file. Nothing was sent.",
            path.display()
        )
    })
}

/// The fork's EVM throwaway admin, made (and recorded) the first time.
pub fn evm_throwaway(fork_root: &Path) -> Result<PrivateKeySigner> {
    let file = evm_throwaway_file(fork_root);
    if std::fs::symlink_metadata(&file).is_err() {
        let key = PrivateKeySigner::random();
        record(
            &evm_throwaway_record(fork_root),
            &format!("{:?}", key.address()),
        )?;
        // Another run may have made it meanwhile: its key is kept.
        if let Err(error) = write_private_new(
            &file,
            &format!("0x{}\n", alloy::hex::encode(key.to_bytes())),
        ) && std::fs::symlink_metadata(&file).is_err()
        {
            return Err(error);
        }
    }
    check_file(&file)?;
    let key = read_evm(&file)?;
    if !recorded(&evm_throwaway_record(fork_root))
        .contains(&format!("{:?}", key.address()).to_ascii_lowercase())
    {
        bail!(
            "{} holds a key this tool didn't record as a throwaway. Nothing was sent.",
            file.display()
        );
    }
    Ok(key)
}

/// The fork's Solana throwaway admin, made (and recorded) the first time.
pub fn solana_throwaway(fork_root: &Path) -> Result<Keypair> {
    let file = solana_throwaway_file(fork_root);
    if std::fs::symlink_metadata(&file).is_err() {
        let pair = Keypair::new();
        record(
            &solana_throwaway_record(fork_root),
            &pair.pubkey().to_string(),
        )?;
        let bytes: Vec<String> = pair.to_bytes().iter().map(u8::to_string).collect();
        if let Err(error) = write_private_new(&file, &format!("[{}]", bytes.join(",")))
            && std::fs::symlink_metadata(&file).is_err()
        {
            return Err(error);
        }
    }
    check_file(&file)?;
    let pair = read_solana(&file)?;
    if !recorded(&solana_throwaway_record(fork_root))
        .contains(&pair.pubkey().to_string().to_ascii_lowercase())
    {
        bail!(
            "{} holds a key this tool didn't record as a throwaway. Nothing was sent.",
            file.display()
        );
    }
    Ok(pair)
}

/// The git directory a `.git` entry (a worktree's file, or a checkout's
/// directory) points to, and its repository's common directory, resolved.
fn git_dirs(dot_git: &Path) -> Option<(PathBuf, PathBuf)> {
    let meta = std::fs::symlink_metadata(dot_git).ok()?;
    let git_dir = if meta.is_dir() {
        dot_git.to_path_buf()
    } else {
        let text = std::fs::read_to_string(dot_git).ok()?;
        let named = PathBuf::from(text.trim().strip_prefix("gitdir:")?.trim());
        if named.is_absolute() {
            named
        } else {
            dot_git.parent()?.join(named)
        }
    };
    let common = match std::fs::read_to_string(git_dir.join("commondir")) {
        Ok(text) => {
            let named = PathBuf::from(text.trim());
            if named.is_absolute() {
                named
            } else {
                git_dir.join(named)
            }
        }
        Err(_) => git_dir.clone(),
    };
    Some((
        std::fs::canonicalize(&git_dir).ok()?,
        std::fs::canonicalize(&common).ok()?,
    ))
}

/// This repository's git common directory: the one every worktree of it
/// shares.
fn common_dir() -> Option<PathBuf> {
    git_dirs(&repository().join(".git")).map(|(_, common)| common)
}

/// Every worktree of this repository git knows of, this one among them:
/// the main checkout (the common directory's parent) and each one listed
/// under the common directory's `worktrees`.
pub fn worktrees() -> Vec<PathBuf> {
    let mut found = vec![repository()];
    let Some(common) = common_dir() else {
        return found;
    };
    if common.file_name().is_some_and(|name| name == ".git")
        && let Some(main) = common.parent()
    {
        found.push(main.to_path_buf());
    }
    if let Ok(entries) = std::fs::read_dir(common.join("worktrees")) {
        for entry in entries.flatten() {
            if let Ok(text) = std::fs::read_to_string(entry.path().join("gitdir"))
                && let Some(tree) = Path::new(text.trim()).parent()
            {
                found.push(tree.to_path_buf());
            }
        }
    }
    found
}

/// Whether `path` is inside a worktree of this repository: one git lists,
/// or any directory above it whose `.git` shares this repository's common
/// directory (a worktree git has forgotten, a copy of the checkout).
pub fn is_in_a_worktree(path: &Path) -> bool {
    if worktrees()
        .iter()
        .any(|tree| mainnet_guard::is_under(path, tree))
    {
        return true;
    }
    let (Some(common), Ok(resolved)) = (common_dir(), mainnet_guard::resolved(path)) else {
        return false;
    };
    resolved
        .ancestors()
        .any(|above| git_dirs(&above.join(".git")).is_some_and(|(_, theirs)| theirs == common))
}

/// The fork-rehearsal directories whose throwaways are refused on mainnet:
/// `fork_root`, and `.shonin/mainnet-fork` in every worktree of this
/// repository (a rehearsal run from another worktree records its own).
fn fork_roots(fork_root: &Path) -> Vec<PathBuf> {
    let mut roots = vec![fork_root.to_path_buf()];
    roots.extend(
        worktrees()
            .into_iter()
            .map(|tree| tree.join(".shonin").join("mainnet-fork")),
    );
    roots
}

/// Whether `address` is one of the EVM throwaway admins recorded under
/// `fork_root` or any worktree's fork directory.
pub fn is_evm_throwaway(fork_root: &Path, address: Address) -> bool {
    let wanted = format!("{address:?}").to_ascii_lowercase();
    fork_roots(fork_root)
        .iter()
        .any(|root| recorded(&evm_throwaway_record(root)).contains(&wanted))
}

/// Whether `key` is one of the Solana throwaway admins recorded under
/// `fork_root` or any worktree's fork directory.
pub fn is_solana_throwaway(fork_root: &Path, key: &Pubkey) -> bool {
    let wanted = key.to_string().to_ascii_lowercase();
    fork_roots(fork_root)
        .iter()
        .any(|root| recorded(&solana_throwaway_record(root)).contains(&wanted))
}

/// Refuse, in every mode but a fork rehearsal, an EVM `ADMIN` whose key
/// anyone holds (an account of the public development mnemonic) or whose key
/// a fork rehearsal made and keeps under `.shonin/mainnet-fork`: the admin
/// is admin of every contract for good, so a deploy naming it, its dry run,
/// its admin steps and its read-back all refuse it.
pub fn refuse_public_evm_admin(mode: Mode, admin: Address) -> Result<()> {
    if mode == Mode::Fork {
        return Ok(());
    }
    if mainnet_guard::is_public_development_address(admin)? {
        bail!(
            "ADMIN={admin} is an account of anvil's public development mnemonic: anyone holds \
             its key, and the admin is admin of every contract for good. Mainnet refuses it (a \
             dry run says so as a real deploy would). Nothing was sent."
        );
    }
    if is_evm_throwaway(&mainnet_guard::fork_root(), admin) {
        bail!(
            "ADMIN={admin} is a fork rehearsal's throwaway admin (its key is kept under \
             .shonin/mainnet-fork): never a mainnet admin. Mainnet refuses it (a dry run says so \
             as a real deploy would). Nothing was sent."
        );
    }
    Ok(())
}

/// How many indices of the public development mnemonic Solana mainnet
/// refuses along each indexed path (`m/44'/501'/<i>'/0'`, the local
/// cluster's and most wallets', and `m/44'/501'/<i>'`, other wallets' and
/// Ledger's): as many as on EVM ([`mainnet_guard::PUBLIC_DEVELOPMENT_ACCOUNTS`]).
pub const PUBLIC_DEVELOPMENT_SOLANA_KEYS: u32 = mainnet_guard::PUBLIC_DEVELOPMENT_ACCOUNTS;

/// HMAC (RFC 2104) over SHA-512.
fn hmac_sha512(key: &[u8], message: &[u8]) -> [u8; 64] {
    use sha2::{Digest, Sha512};
    let mut block = [0u8; 128];
    if key.len() > block.len() {
        block[..64].copy_from_slice(&Sha512::digest(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let pad = |byte: u8| block.iter().map(|b| b ^ byte).collect::<Vec<u8>>();
    let inner = Sha512::new()
        .chain_update(pad(0x36))
        .chain_update(message)
        .finalize();
    Sha512::new()
        .chain_update(pad(0x5c))
        .chain_update(inner)
        .finalize()
        .into()
}

/// SLIP-0010's ed25519 key at `path` (every step hardened) from the
/// master key's secret and chain code.
fn slip10_ed25519(master: &[u8; 64], path: &[u32]) -> Pubkey {
    let (mut key, mut chain) = (master[..32].to_vec(), master[32..].to_vec());
    for step in path {
        let mut data = vec![0];
        data.extend_from_slice(&key);
        data.extend_from_slice(&(step | 0x8000_0000u32).to_be_bytes());
        let child = hmac_sha512(&chain, &data);
        (key, chain) = (child[..32].to_vec(), child[32..].to_vec());
    }
    let mut secret = [0u8; 32];
    secret.copy_from_slice(&key);
    Keypair::new_from_array(secret).pubkey()
}

/// The public development mnemonic's Solana keys along every common path:
/// the seed's first 32 bytes (`solana-keygen recover` with no derivation
/// path), `m/44'/501'`, and the first `count` indices of `m/44'/501'/<i>'/0'`
/// and of `m/44'/501'/<i>'`. The seed and the master key are derived once.
fn public_development_solana_keys(count: u32) -> Result<BTreeSet<Pubkey>> {
    use alloy::signers::local::coins_bip39::{English, Mnemonic};
    let seed = Mnemonic::<English>::new_from_phrase(chain::solana::keys::DEVELOPMENT_MNEMONIC)
        .ok()
        .and_then(|mnemonic| mnemonic.to_seed(None).ok())
        .ok_or_else(|| anyhow!("the public development mnemonic gives no seed"))?;
    let mut prefix = [0u8; 32];
    prefix.copy_from_slice(&seed[..32]);
    let master = hmac_sha512(b"ed25519 seed", &seed);
    let mut keys = BTreeSet::from([
        Keypair::new_from_array(prefix).pubkey(),
        slip10_ed25519(&master, &[44, 501]),
    ]);
    for index in 0..count {
        keys.insert(slip10_ed25519(&master, &[44, 501, index, 0]));
        keys.insert(slip10_ed25519(&master, &[44, 501, index]));
    }
    Ok(keys)
}

/// Whether `key` is one of the public development mnemonic's Solana keys
/// (with no derivation path, at `m/44'/501'`, or among the first
/// [`PUBLIC_DEVELOPMENT_SOLANA_KEYS`] indices of either indexed path):
/// anyone holds it. They are derived once per run, when first needed.
pub fn is_public_development_solana_key(key: &Pubkey) -> Result<bool> {
    static PUBLIC: std::sync::OnceLock<BTreeSet<Pubkey>> = std::sync::OnceLock::new();
    if let Some(public) = PUBLIC.get() {
        return Ok(public.contains(key));
    }
    let derived = public_development_solana_keys(PUBLIC_DEVELOPMENT_SOLANA_KEYS)?;
    Ok(PUBLIC.get_or_init(|| derived).contains(key))
}

/// Refuse, in every mode but a fork rehearsal, a Solana `ADMIN` that is a
/// key of the public development mnemonic or one a fork rehearsal made: as
/// [`refuse_public_evm_admin`].
pub fn refuse_public_solana_admin(mode: Mode, admin: &Pubkey) -> Result<()> {
    if mode != Mode::Fork && is_public_development_solana_key(admin)? {
        bail!(
            "ADMIN={admin} is a key of the public development mnemonic: anyone holds it, and \
             the admin is admin of every program for good. Mainnet refuses it (a dry run says \
             so as a real deploy would). Nothing was sent."
        );
    }
    if mode != Mode::Fork && is_solana_throwaway(&mainnet_guard::fork_root(), admin) {
        bail!(
            "ADMIN={admin} is a fork rehearsal's throwaway admin (its keypair is kept under \
             .shonin/mainnet-fork): never a mainnet admin. Mainnet refuses it (a dry run says so \
             as a real deploy would). Nothing was sent."
        );
    }
    Ok(())
}

/// The EVM admin's key for `mode`, checked: on a fork the throwaway (made
/// the first time) unless a file is named, which must be the throwaway or
/// one of anvil's development keys; on mainnet the file named, held to
/// every rule in this module's header. Its address must be `admin`'s and
/// none of `deployers`. A dry run reads no key.
pub fn evm_admin_key(
    mode: Mode,
    path: Option<&Path>,
    admin: Address,
    deployers: &[Address],
    fork_root: &Path,
) -> Result<Option<PrivateKeySigner>> {
    let key = match (mode, path) {
        (Mode::DryRun, None) => return Ok(None),
        (Mode::DryRun, Some(_)) => bail!(
            "ADMIN_KEYFILE without CONFIRM_MAINNET: a send on mainnet takes both. Nothing was sent."
        ),
        (Mode::Fork, None) => evm_throwaway(fork_root)?,
        (Mode::Fork, Some(path)) => {
            check_file(path)?;
            let key = read_evm(path)?;
            let anvil = mainnet_guard::anvil_addresses()?;
            if !is_evm_throwaway(fork_root, key.address()) && !anvil.contains(&key.address()) {
                bail!(
                    "a fork rehearsal signs the admin's steps with its throwaway admin (leave \
                     ADMIN_KEYFILE out) or one of anvil's public development keys, never another: \
                     {} holds the key of {}, whose transactions on a fork would be valid on \
                     mainnet. Nothing was sent.",
                    path.display(),
                    key.address()
                );
            }
            key
        }
        (Mode::Send, None) => bail!(
            "sending the admin's steps on mainnet takes ADMIN_KEYFILE=<path>. Nothing was sent."
        ),
        (Mode::Send, Some(path)) => {
            check_file(path)?;
            check_mainnet_place(path)?;
            let key = read_evm(path)?;
            if mainnet_guard::is_public_development_address(key.address())? {
                bail!(
                    "ADMIN_KEYFILE holds a key of anvil's public development mnemonic ({}): \
                     anyone holds it. Nothing was sent.",
                    key.address()
                );
            }
            if is_evm_throwaway(fork_root, key.address()) {
                bail!(
                    "ADMIN_KEYFILE holds a fork rehearsal's throwaway admin ({}), never a \
                     mainnet key. Nothing was sent.",
                    key.address()
                );
            }
            key
        }
    };
    if key.address() != admin {
        bail!(
            "ADMIN_KEYFILE holds the key of {}, not ADMIN={admin}. Nothing was sent.",
            key.address()
        );
    }
    if deployers.contains(&key.address()) {
        bail!(
            "ADMIN_KEYFILE holds the deployer's key ({}): the admin and the deployer are two \
             keys. Nothing was sent.",
            key.address()
        );
    }
    Ok(Some(key))
}

/// The Solana admin's keypair for `mode`, checked as [`evm_admin_key`]
/// checks the EVM one (a fork takes its throwaway alone).
pub fn solana_admin_key(
    mode: Mode,
    path: Option<&Path>,
    admin: &Pubkey,
    deployers: &[Pubkey],
    fork_root: &Path,
) -> Result<Option<Keypair>> {
    let pair = match (mode, path) {
        (Mode::DryRun, None) => return Ok(None),
        (Mode::DryRun, Some(_)) => bail!(
            "ADMIN_KEYFILE without CONFIRM_MAINNET: a send on mainnet takes both. Nothing was sent."
        ),
        (Mode::Fork, None) => solana_throwaway(fork_root)?,
        (Mode::Fork, Some(path)) => {
            check_file(path)?;
            let pair = read_solana(path)?;
            if !is_solana_throwaway(fork_root, &pair.pubkey()) {
                bail!(
                    "a fork rehearsal signs the admin's steps with its throwaway admin (leave \
                     ADMIN_KEYFILE out, or name {}), never another: {} holds {}, whose \
                     transactions on a fork would be valid on mainnet. Nothing was sent.",
                    solana_throwaway_file(fork_root).display(),
                    path.display(),
                    pair.pubkey()
                );
            }
            pair
        }
        (Mode::Send, None) => bail!(
            "sending the admin's steps on mainnet takes ADMIN_KEYFILE=<path>. Nothing was sent."
        ),
        (Mode::Send, Some(path)) => {
            check_file(path)?;
            check_mainnet_place(path)?;
            let pair = read_solana(path)?;
            if is_public_development_solana_key(&pair.pubkey())? {
                bail!(
                    "ADMIN_KEYFILE holds a key of the public development mnemonic ({}): anyone \
                     holds it. Nothing was sent.",
                    pair.pubkey()
                );
            }
            if is_solana_throwaway(fork_root, &pair.pubkey()) {
                bail!(
                    "ADMIN_KEYFILE holds a fork rehearsal's throwaway admin ({}), never a \
                     mainnet key. Nothing was sent.",
                    pair.pubkey()
                );
            }
            pair
        }
    };
    if pair.pubkey() != *admin {
        bail!(
            "ADMIN_KEYFILE holds {}, not ADMIN={admin}. Nothing was sent.",
            pair.pubkey()
        );
    }
    if deployers.contains(&pair.pubkey()) {
        bail!(
            "ADMIN_KEYFILE holds the deployer's key ({}): the admin and the deployer are two \
             keys. Nothing was sent.",
            pair.pubkey()
        );
    }
    Ok(Some(pair))
}

/// What a real deploy asks of `ADMIN_KEYFILE`, the file the admin's steps
/// will sign with: given beside `ADMIN`, it proves before anything is sent
/// that the key of the address made admin of every contract for good
/// exists, outside this repository, readable by its owner alone, and is
/// neither the deployer's nor a public or throwaway key (every rule of
/// [`evm_admin_key`] on mainnet). A dry run checks it when it is given; a
/// fork rehearsal holds it to the fork's rules. The key is read only for
/// its address: the deploy never signs with it.
pub fn check_deploy_evm_admin(
    mode: Mode,
    path: Option<&Path>,
    admin: Address,
    deployers: &[Address],
    fork_root: &Path,
) -> Result<()> {
    match (mode, path) {
        (Mode::DryRun | Mode::Fork, None) => Ok(()),
        (Mode::Send, None) => bail!("{}", DEPLOY_WITHOUT_ADMIN_KEYFILE),
        (Mode::Fork, Some(path)) => {
            evm_admin_key(Mode::Fork, Some(path), admin, deployers, fork_root).map(drop)
        }
        (_, Some(path)) => {
            evm_admin_key(Mode::Send, Some(path), admin, deployers, fork_root).map(drop)
        }
    }
}

/// [`check_deploy_evm_admin`] for the Solana deploy's admin keypair file.
pub fn check_deploy_solana_admin(
    mode: Mode,
    path: Option<&Path>,
    admin: &Pubkey,
    deployers: &[Pubkey],
    fork_root: &Path,
) -> Result<()> {
    match (mode, path) {
        (Mode::DryRun | Mode::Fork, None) => Ok(()),
        (Mode::Send, None) => bail!("{}", DEPLOY_WITHOUT_ADMIN_KEYFILE),
        (Mode::Fork, Some(path)) => {
            solana_admin_key(Mode::Fork, Some(path), admin, deployers, fork_root).map(drop)
        }
        (_, Some(path)) => {
            solana_admin_key(Mode::Send, Some(path), admin, deployers, fork_root).map(drop)
        }
    }
}

const DEPLOY_WITHOUT_ADMIN_KEYFILE: &str = "a real deploy takes ADMIN_KEYFILE=<the admin's key \
     file> beside ADMIN: ADMIN becomes admin of every contract for good, so the deploy first \
     checks that its key exists, outside this repository and readable by its owner alone (the \
     same file the admin's steps sign with). Nothing was sent.";

/// Every form a secret's bytes may be written in: hex with and without
/// `0x` (any letter case is matched), base58, and the decimal list a
/// keypair file holds; for a 64-byte ed25519 keypair, its 32-byte seed's
/// forms as well.
fn secret_forms(secret: &[u8]) -> Vec<String> {
    let forms_of = |bytes: &[u8]| {
        let decimal: Vec<String> = bytes.iter().map(u8::to_string).collect();
        vec![
            alloy::hex::encode(bytes),
            bs58::encode(bytes).into_string(),
            decimal.join(","),
            decimal.join(", "),
        ]
    };
    let mut forms = forms_of(secret);
    // An ed25519 keypair's first 32 bytes are its secret seed, a key of its
    // own: each of its forms is redacted too (after the keypair's, which
    // hold it).
    if secret.len() == 64 {
        forms.extend(forms_of(&secret[..32]));
    }
    forms
}

/// `text` with every form of `secret` in it replaced: what is printed or
/// returned from a send that signed with it.
pub fn redact(text: &str, secret: &[u8]) -> String {
    let mut out = text.to_string();
    for form in secret_forms(secret) {
        if form.is_empty() {
            continue;
        }
        loop {
            let lower = out.to_ascii_lowercase();
            let needle = form.to_ascii_lowercase();
            // Base58 is case-sensitive; hex and decimals are not.
            let at = if form
                .chars()
                .all(|c| c.is_ascii_hexdigit() || c == ',' || c == ' ')
            {
                lower.find(&needle)
            } else {
                out.find(&form)
            };
            match at {
                Some(at) => out.replace_range(at..at + form.len(), "[redacted]"),
                None => break,
            }
        }
    }
    out
}

/// An error from a send signed with `secret`, redacted ([`redact`]) in
/// every line of its chain.
pub fn redacted(error: anyhow::Error, secret: &[u8]) -> anyhow::Error {
    anyhow!(redact(&format!("{error:#}"), secret))
}

/// The most gas one admin transaction may carry on `chain_id`: Robinhood
/// Chain (Arbitrum Nitro) 32,000,000; HyperEVM its small block's limit,
/// [`crate::deploy::HYPEREVM_SMALL_BLOCK_GAS`] (the admin's address never
/// asks for big blocks); every other chain EIP-7825's cap.
pub fn admin_tx_gas_cap(chain_id: u64) -> u64 {
    match chain_id {
        4663 | 46630 => 32_000_000,
        999 | 998 => crate::deploy::HYPEREVM_SMALL_BLOCK_GAS,
        _ => 16_777_216,
    }
}

/// The gas limit an admin step is sent with: its estimate and a fifth more,
/// within the chain's cap; nothing when the estimate is over the cap.
pub fn admin_gas_limit(estimate: u64, cap: u64) -> Option<u64> {
    (estimate <= cap).then(|| (estimate + estimate / 5 + 10_000).min(cap))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("shonin-admin-key-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn private(file: &Path, text: &str) {
        write_private_new(file, text).unwrap();
    }

    #[test]
    fn mainnet_refuses_any_key_of_the_public_development_mnemonic_not_only_anvil_s_first_ten() {
        let phrase = "test test test test test test test test test test test junk";
        let evm = chain::keys_from_mnemonic(phrase, 12).unwrap();
        for key in &evm {
            assert!(mainnet_guard::is_public_development_address(key.address()).unwrap());
            assert!(refuse_public_evm_admin(Mode::DryRun, key.address()).is_err());
            assert!(refuse_public_evm_admin(Mode::Send, key.address()).is_err());
            assert!(refuse_public_evm_admin(Mode::Fork, key.address()).is_ok());
        }
        let last = alloy::signers::local::MnemonicBuilder::<
            alloy::signers::local::coins_bip39::English,
        >::default()
        .phrase(phrase)
        .index(mainnet_guard::PUBLIC_DEVELOPMENT_ACCOUNTS - 1)
        .unwrap()
        .build()
        .unwrap();
        assert!(mainnet_guard::is_public_development_address(last.address()).unwrap());
        for index in [0, 1, mainnet_guard::PUBLIC_DEVELOPMENT_ACCOUNTS - 1] {
            let ledger_live = alloy::signers::local::MnemonicBuilder::<
                alloy::signers::local::coins_bip39::English,
            >::default()
            .phrase(phrase)
            .derivation_path(format!("m/44'/60'/{index}'/0/0"))
            .unwrap()
            .build()
            .unwrap();
            assert!(
                mainnet_guard::is_public_development_address(ledger_live.address()).unwrap(),
                "Ledger Live's account {index}"
            );
            assert!(refuse_public_evm_admin(Mode::DryRun, ledger_live.address()).is_err());
        }
        let other = PrivateKeySigner::random().address();
        assert!(!mainnet_guard::is_public_development_address(other).unwrap());
        assert!(refuse_public_evm_admin(Mode::Send, other).is_ok());

        use chain::solana::keys::{DEVELOPMENT_MNEMONIC, keypair_from_mnemonic};
        for index in [0, 1, 10, 111, PUBLIC_DEVELOPMENT_SOLANA_KEYS - 1] {
            let key = keypair_from_mnemonic(DEVELOPMENT_MNEMONIC, index)
                .unwrap()
                .pubkey();
            let refusal = refuse_public_solana_admin(Mode::DryRun, &key)
                .unwrap_err()
                .to_string();
            assert!(refusal.contains("public development mnemonic"), "{refusal}");
            assert!(refuse_public_solana_admin(Mode::Fork, &key).is_ok());
        }
        let seed = alloy::signers::local::coins_bip39::Mnemonic::<
            alloy::signers::local::coins_bip39::English,
        >::new_from_phrase(DEVELOPMENT_MNEMONIC)
        .unwrap()
        .to_seed(None)
        .unwrap();
        let mut prefix = [0u8; 32];
        prefix.copy_from_slice(&seed[..32]);
        assert!(
            is_public_development_solana_key(&Keypair::new_from_array(prefix).pubkey()).unwrap()
        );
        assert!(refuse_public_solana_admin(Mode::Send, &Keypair::new().pubkey()).is_ok());
    }

    #[test]
    fn an_admin_key_file_readable_by_others_or_through_a_link_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("file");
        let key = PrivateKeySigner::random();
        let file = dir.join("admin");
        private(&file, &format!("0x{}", alloy::hex::encode(key.to_bytes())));
        assert!(check_file(&file).is_ok());
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o640)).unwrap();
        assert!(
            check_file(&file)
                .unwrap_err()
                .to_string()
                .contains("readable by others")
        );
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o604)).unwrap();
        assert!(check_file(&file).is_err());
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
        let link = dir.join("link");
        std::os::unix::fs::symlink(&file, &link).unwrap();
        assert!(
            check_file(&link)
                .unwrap_err()
                .to_string()
                .contains("symlink")
        );
        let hard = dir.join("hard");
        std::fs::hard_link(&file, &hard).unwrap();
        assert!(
            check_file(&hard)
                .unwrap_err()
                .to_string()
                .contains("hard link")
        );
        assert!(check_file(&dir.join("missing")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_mainnet_admin_key_file_inside_the_repository_or_a_shonin_directory_is_refused() {
        assert!(check_mainnet_place(&repository().join("admin-key")).is_err());
        assert!(check_mainnet_place(&repository().join(".shonin/x/admin-key")).is_err());
        let dir = scratch("place");
        assert!(check_mainnet_place(&dir.join("admin-key")).is_ok());
        assert!(check_mainnet_place(&dir.join(".shonin/admin-key")).is_err());
        assert!(check_mainnet_place(&dir.join(".Shonin/admin-key")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_admin_key_must_be_admin_s_and_not_the_deployer_s_nor_a_public_or_throwaway_key_on_mainnet()
     {
        let dir = scratch("send");
        let fork_root = dir.join("fork");
        let key = PrivateKeySigner::random();
        let file = dir.join("admin");
        private(&file, &format!("0x{}", alloy::hex::encode(key.to_bytes())));
        let ok = evm_admin_key(Mode::Send, Some(&file), key.address(), &[], &fork_root);
        assert_eq!(ok.unwrap().unwrap().address(), key.address());
        let other = Address::repeat_byte(7);
        let error = evm_admin_key(Mode::Send, Some(&file), other, &[], &fork_root).unwrap_err();
        assert!(error.to_string().contains("not ADMIN"), "{error}");
        let error = evm_admin_key(
            Mode::Send,
            Some(&file),
            key.address(),
            &[key.address()],
            &fork_root,
        )
        .unwrap_err();
        assert!(error.to_string().contains("deployer"), "{error}");
        // A fork's throwaway is refused on mainnet, wherever its file is.
        let throwaway = evm_throwaway(&fork_root).unwrap();
        let copy = dir.join("copy");
        private(
            &copy,
            &format!("0x{}", alloy::hex::encode(throwaway.to_bytes())),
        );
        let error = evm_admin_key(
            Mode::Send,
            Some(&copy),
            throwaway.address(),
            &[],
            &fork_root,
        )
        .unwrap_err();
        assert!(error.to_string().contains("throwaway"), "{error}");
        // Anvil's first public key.
        let anvil = &chain::keys_from_mnemonic(
            "test test test test test test test test test test test junk",
            1,
        )
        .unwrap()[0];
        let public = dir.join("public");
        private(
            &public,
            &format!("0x{}", alloy::hex::encode(anvil.to_bytes())),
        );
        let error =
            evm_admin_key(Mode::Send, Some(&public), anvil.address(), &[], &fork_root).unwrap_err();
        assert!(error.to_string().contains("anvil"), "{error}");
        // A dry run reads no key, and refuses one named.
        assert!(
            evm_admin_key(Mode::DryRun, None, key.address(), &[], &fork_root)
                .unwrap()
                .is_none()
        );
        assert!(evm_admin_key(Mode::DryRun, Some(&file), key.address(), &[], &fork_root).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_fork_signs_the_admin_s_steps_with_its_throwaway_or_an_anvil_key_alone() {
        let dir = scratch("fork");
        let fork_root = dir.join("fork");
        let made = evm_throwaway(&fork_root).unwrap();
        let again = evm_throwaway(&fork_root).unwrap();
        assert_eq!(made.address(), again.address(), "kept across runs");
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(evm_throwaway_file(&fork_root))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        let taken = evm_admin_key(Mode::Fork, None, made.address(), &[], &fork_root).unwrap();
        assert_eq!(taken.unwrap().address(), made.address());
        let real = PrivateKeySigner::random();
        let file = dir.join("real");
        private(&file, &format!("0x{}", alloy::hex::encode(real.to_bytes())));
        let error =
            evm_admin_key(Mode::Fork, Some(&file), real.address(), &[], &fork_root).unwrap_err();
        assert!(error.to_string().contains("throwaway"), "{error}");
        // Solana: the throwaway alone.
        let pair = solana_throwaway(&fork_root).unwrap();
        let taken = solana_admin_key(Mode::Fork, None, &pair.pubkey(), &[], &fork_root).unwrap();
        assert_eq!(taken.unwrap().pubkey(), pair.pubkey());
        let other = Keypair::new();
        let file = dir.join("other.json");
        let bytes: Vec<String> = other.to_bytes().iter().map(u8::to_string).collect();
        private(&file, &format!("[{}]", bytes.join(",")));
        assert!(
            solana_admin_key(Mode::Fork, Some(&file), &other.pubkey(), &[], &fork_root).is_err()
        );
        let copy = dir.join("copy.json");
        let bytes: Vec<String> = pair.to_bytes().iter().map(u8::to_string).collect();
        private(&copy, &format!("[{}]", bytes.join(",")));
        let error =
            solana_admin_key(Mode::Send, Some(&copy), &pair.pubkey(), &[], &fork_root).unwrap_err();
        assert!(error.to_string().contains("throwaway"), "{error}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_key_is_redacted_in_every_form_it_may_be_written_in() {
        let key = PrivateKeySigner::random();
        let bytes = key.to_bytes();
        let hex = alloy::hex::encode(bytes);
        for text in [
            format!("sending 0x{hex} failed"),
            format!("x {} y", hex.to_ascii_uppercase()),
            format!("{} trailing", bs58::encode(bytes).into_string()),
        ] {
            let out = redact(&text, bytes.as_slice());
            assert!(!out.to_ascii_lowercase().contains(&hex), "{out}");
            assert!(!out.contains(&bs58::encode(bytes).into_string()), "{out}");
            assert!(out.contains("[redacted]"), "{out}");
        }
    }

    #[test]
    fn an_admin_step_over_the_chain_s_per_transaction_cap_is_not_sent() {
        assert_eq!(admin_tx_gas_cap(999), 2_000_000);
        assert_eq!(admin_tx_gas_cap(4663), 32_000_000);
        assert_eq!(admin_tx_gas_cap(8453), 16_777_216);
        assert_eq!(admin_gas_limit(3_000_001, 3_000_000), None);
        assert_eq!(admin_gas_limit(2_900_000, 3_000_000), Some(3_000_000));
        assert_eq!(admin_gas_limit(100_000, 3_000_000), Some(130_000));
    }

    #[test]
    fn a_solana_keypair_s_secret_seed_is_redacted_in_every_form_too() {
        let pair = Keypair::new();
        let bytes = pair.to_bytes();
        let seed = &bytes[..32];
        let hex = alloy::hex::encode(seed);
        let base58 = bs58::encode(seed).into_string();
        let decimal: Vec<String> = seed.iter().map(u8::to_string).collect();
        for text in [
            format!("seed 0x{hex}"),
            format!("seed {}", hex.to_ascii_uppercase()),
            format!("seed {base58} after"),
            format!("[{}]", decimal.join(",")),
            format!("[{}]", decimal.join(", ")),
        ] {
            let out = redact(&text, &bytes);
            assert!(!out.to_ascii_lowercase().contains(&hex), "{out}");
            assert!(!out.contains(&base58), "{out}");
            assert!(!out.contains(&decimal.join(",")), "{out}");
            assert!(!out.contains(&decimal.join(", ")), "{out}");
            assert!(out.contains("[redacted]"), "{out}");
        }
    }

    #[test]
    fn hyperevm_s_admin_cap_is_a_small_block_s_gas() {
        assert_eq!(
            admin_tx_gas_cap(999),
            crate::deploy::HYPEREVM_SMALL_BLOCK_GAS
        );
        assert_eq!(crate::deploy::HYPEREVM_SMALL_BLOCK_GAS, 2_000_000);
        assert_eq!(admin_gas_limit(2_000_001, admin_tx_gas_cap(999)), None);
    }

    #[test]
    fn a_mainnet_admin_key_file_in_any_checkout_of_this_repository_is_refused() {
        // A directory whose `.git` names this repository's git directory: a
        // checkout of it (as a worktree is) outside this one.
        let dir = scratch("other-checkout");
        let common = common_dir().expect("this repository's git directory");
        std::fs::write(dir.join(".git"), format!("gitdir: {}\n", common.display())).unwrap();
        let file = dir.join("keys").join("admin");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        private(&file, "0x01\n");
        assert!(is_in_a_worktree(&file));
        let refusal = check_mainnet_place(&file).unwrap_err().to_string();
        assert!(refusal.contains("worktree"), "{refusal}");
        // A directory of another repository, or none, is not refused.
        std::fs::write(dir.join(".git"), "gitdir: /nowhere/at/all\n").unwrap();
        assert!(!is_in_a_worktree(&file));
        assert!(check_mainnet_place(&file).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn mainnet_refuses_a_public_or_throwaway_admin_in_every_mode_but_a_fork() {
        let anvil = mainnet_guard::anvil_addresses().unwrap()[0];
        let throwaway = evm_throwaway(&mainnet_guard::fork_root())
            .unwrap()
            .address();
        let solana = solana_throwaway(&mainnet_guard::fork_root())
            .unwrap()
            .pubkey();
        for mode in [Mode::DryRun, Mode::Send] {
            for admin in [anvil, throwaway] {
                let refusal = refuse_public_evm_admin(mode, admin)
                    .unwrap_err()
                    .to_string();
                assert!(refusal.contains("Nothing was sent"), "{refusal}");
            }
            assert!(refuse_public_solana_admin(mode, &solana).is_err());
            assert!(refuse_public_evm_admin(mode, Address::repeat_byte(0x5a)).is_ok());
            assert!(refuse_public_solana_admin(mode, &Pubkey::new_unique()).is_ok());
        }
        assert!(refuse_public_evm_admin(Mode::Fork, anvil).is_ok());
        assert!(refuse_public_evm_admin(Mode::Fork, throwaway).is_ok());
        assert!(refuse_public_solana_admin(Mode::Fork, &solana).is_ok());
    }

    #[test]
    fn a_real_deploy_takes_the_admin_s_key_file_and_holds_it_to_the_mainnet_rules() {
        let fork_root = scratch("deploy-fork-root");
        let admin = PrivateKeySigner::random();
        // Without it, a real deploy is refused; a dry run and a fork go on.
        assert!(
            check_deploy_evm_admin(Mode::Send, None, admin.address(), &[], &fork_root)
                .unwrap_err()
                .to_string()
                .contains("ADMIN_KEYFILE")
        );
        assert!(
            check_deploy_evm_admin(Mode::DryRun, None, admin.address(), &[], &fork_root).is_ok()
        );
        assert!(check_deploy_evm_admin(Mode::Fork, None, admin.address(), &[], &fork_root).is_ok());
        let solana = Keypair::new();
        assert!(
            check_deploy_solana_admin(Mode::Send, None, &solana.pubkey(), &[], &fork_root).is_err()
        );
        // ADMIN's own file, outside the repository, 0600: taken, in a dry
        // run too; another key's, or the deployer's, refused.
        let dir = scratch("deploy-key");
        let file = dir.join("admin");
        private(
            &file,
            &format!("0x{}\n", alloy::hex::encode(admin.to_bytes())),
        );
        for mode in [Mode::Send, Mode::DryRun] {
            assert!(
                check_deploy_evm_admin(mode, Some(&file), admin.address(), &[], &fork_root).is_ok()
            );
            assert!(
                check_deploy_evm_admin(
                    mode,
                    Some(&file),
                    Address::repeat_byte(0x42),
                    &[],
                    &fork_root
                )
                .is_err()
            );
            assert!(
                check_deploy_evm_admin(
                    mode,
                    Some(&file),
                    admin.address(),
                    &[admin.address()],
                    &fork_root
                )
                .is_err()
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&fork_root);
    }
}
