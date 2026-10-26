//! The Solana mainnet commands refuse what their rules forbid before they
//! send or write anything. Each test runs the devnet binary; those that
//! need a cluster talk to a fake JSON-RPC node on a loopback port (18740,
//! 18741)
//! that answers as a Surfpool fork of mainnet-beta. Nothing reaches a public
//! endpoint.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use chain::solana::{Keypair, Pubkey, Signer, keys};

const SHIPPING: [&str; 8] = [
    "shonin_cs_verifier",
    "shonin_orca_driver",
    "shonin_profiles",
    "shonin_registry",
    "shonin_publishing",
    "shonin_prices",
    "shonin_station",
    "shonin_vault",
];

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "solana-mainnet-refusals-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn devnet(args: &[&str], envs: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_devnet"));
    cmd.current_dir(repo())
        .env_remove("SHONIN_RPC_SOLANA_MAINNET_BETA")
        .env_remove("SHONIN_NETWORK")
        .env_remove("SHONIN_CHAINS")
        .args([
            "--store",
            std::env::temp_dir()
                .join("solana-mainnet-refusals-store")
                .to_str()
                .unwrap(),
            "--artifacts",
            std::env::temp_dir()
                .join("solana-mainnet-refusals-artifacts")
                .to_str()
                .unwrap(),
        ])
        .args(args);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    cmd.output().expect("running the devnet binary")
}

fn text(out: &Output) -> String {
    format!(
        "status {:?}\nstdout:\n{}\nstderr:\n{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn keypair_dir(dir: &Path) {
    for name in SHIPPING {
        keys::write_keypair(&Keypair::new(), &dir.join(format!("{name}-keypair.json"))).unwrap();
    }
}

/// An ADMIN that is not on the ed25519 curve (a program-derived address,
/// which no key can ever sign for) is refused: on a real send it would be
/// the registry's, the price list's and the vault factory's admin for good.
#[test]
fn an_admin_no_key_can_sign_for_is_refused() {
    let dir = scratch("offcurve");
    let keypairs = dir.join("keys");
    keypair_dir(&keypairs);
    // The registry config's address: a PDA, off the curve.
    let (pda, _) = Pubkey::find_program_address(&[b"config"], &Pubkey::new_unique());
    assert!(!pda.is_on_curve());
    let out = devnet(
        &[
            "--network",
            "mainnet",
            "--deployments-dir",
            dir.join("deployments").to_str().unwrap(),
            "--vault-chain",
            "solana",
            "mainnet-solana-ids",
            "--admin",
            &pda.to_string(),
            "--on-fork",
            "--program-keypairs",
            keypairs.to_str().unwrap(),
            "--stand-ins",
            dir.join("stand-ins").to_str().unwrap(),
            "--out",
            dir.join("ids").to_str().unwrap(),
        ],
        &[],
    );
    assert!(
        !out.status.success(),
        "an off-curve ADMIN ({pda}) was accepted:\n{}",
        text(&out)
    );
}

// --- A fake node answering as a Surfpool fork of mainnet-beta -------------------

fn b64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::new();
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                s.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                s.push('=');
            }
        }
    }
    s
}

fn answer(method: &str, params: &serde_json::Value) -> serde_json::Value {
    use serde_json::json;
    match method {
        "getGenesisHash" => json!("5eykt4UsFv8P8NJdTREpY1vzqKqZKvdpKuc147dw2N9d"),
        "getVersion" => json!({"surfnet-version": "1.6.0", "solana-core": "4.2.1"}),
        "getAccountInfo" => {
            let address = params[0].as_str().unwrap_or_default();
            if address == "SysvarC1ock11111111111111111111111111111111" {
                let mut data = vec![0u8; 40];
                data[32..40].copy_from_slice(&1_761_300_000i64.to_le_bytes());
                json!({"context": {"slot": 1}, "value": {
                    "lamports": 1_169_280u64,
                    "owner": "Sysvar1111111111111111111111111111111111111",
                    "executable": false,
                    "rentEpoch": 0,
                    "space": 40,
                    "data": [b64(&data), "base64"]
                }})
            } else {
                json!({"context": {"slot": 1}, "value": null})
            }
        }
        "getSlot" => json!(1),
        _ => json!(null),
    }
}

/// Serve JSON-RPC on 127.0.0.1:`port` until the test ends.
fn fake_surfpool(port: u16) {
    let listener = TcpListener::bind(("127.0.0.1", port)).expect("binding the fake node");
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            std::thread::spawn(move || {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut stream = stream;
                loop {
                    let mut length = 0usize;
                    let mut line = String::new();
                    loop {
                        line.clear();
                        if reader.read_line(&mut line).unwrap_or(0) == 0 {
                            return;
                        }
                        let l = line.trim_end();
                        if l.is_empty() {
                            break;
                        }
                        if let Some(v) = l
                            .to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|v| v.trim().to_string())
                        {
                            length = v.parse().unwrap_or(0);
                        }
                    }
                    let mut body = vec![0u8; length];
                    if reader.read_exact(&mut body).is_err() {
                        return;
                    }
                    let request: serde_json::Value =
                        serde_json::from_slice(&body).unwrap_or_default();
                    let result = answer(
                        request["method"].as_str().unwrap_or_default(),
                        &request["params"],
                    );
                    let reply = serde_json::json!({
                        "jsonrpc": "2.0", "id": request["id"], "result": result
                    })
                    .to_string();
                    let head = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
                        reply.len()
                    );
                    if stream.write_all(head.as_bytes()).is_err()
                        || stream.write_all(reply.as_bytes()).is_err()
                    {
                        return;
                    }
                }
            });
        }
    });
}

/// Without ON_FORK=1 the hardware key's admin steps are read from mainnet
/// alone: a local Surfpool fork is refused and nothing is written.
#[test]
fn the_hardware_key_s_admin_steps_are_never_read_from_a_fork() {
    let port = 18740;
    fake_surfpool(port);
    let dir = scratch("adminsteps");
    let steps = dir.join("admin-steps");
    let out = devnet(
        &[
            "--network",
            "mainnet",
            "--chains",
            "config/chains.toml",
            "--deployments-dir",
            dir.join("deployments").to_str().unwrap(),
            "--vault-chain",
            "solana",
            "mainnet-solana-admin-steps",
            "--admin",
            &Keypair::new().pubkey().to_string(),
            "--admin-steps-dir",
            steps.to_str().unwrap(),
        ],
        &[(
            "SHONIN_RPC_SOLANA_MAINNET_BETA",
            &format!("http://127.0.0.1:{port}"),
        )],
    );
    let written = steps.join("admin-steps-solana-mainnet-beta.json");
    assert!(
        !out.status.success() && !written.exists(),
        "the hardware key's admin steps were read from a local Surfpool fork and written to {}:\n{}",
        written.display(),
        text(&out)
    );
}

/// A fork rehearsal never takes program keypairs from a directory outside
/// `.shonin/mainnet-fork`, where the owner's real ones would be: the ids
/// step is refused and writes no list of ids for the build.
#[test]
fn a_fork_rehearsal_refuses_program_keypairs_from_outside_its_own_directory() {
    let dir = scratch("ownerkeys");
    let keypairs = dir.join("owner-keys");
    keypair_dir(&keypairs);
    let out_file = dir.join("ids");
    let out = devnet(
        &[
            "--network",
            "mainnet",
            "--deployments-dir",
            dir.join("deployments").to_str().unwrap(),
            "--vault-chain",
            "solana",
            "mainnet-solana-ids",
            "--admin",
            &Keypair::new().pubkey().to_string(),
            "--on-fork",
            "--program-keypairs",
            keypairs.to_str().unwrap(),
            "--stand-ins",
            dir.join("stand-ins").to_str().unwrap(),
            "--out",
            out_file.to_str().unwrap(),
        ],
        &[],
    );
    assert!(
        !out.status.success()
            && !out_file.exists()
            && String::from_utf8_lossy(&out.stderr).contains("never come onto a fork"),
        "a fork rehearsal took program keypairs from {}:\n{}",
        keypairs.display(),
        text(&out)
    );
}

/// A mainnet deploy is given the verifiable build alone: programs without
/// its record (a host `cargo build-sbf`'s output) are refused before any
/// cluster is asked or anything written.
#[test]
fn a_deploy_of_programs_that_are_not_the_verifiable_build_is_refused() {
    let dir = scratch("hostbuild");
    let programs = dir.join("programs-built");
    std::fs::create_dir_all(&programs).unwrap();
    for name in SHIPPING {
        std::fs::write(programs.join(format!("{name}.so")), b"a host build").unwrap();
    }
    let steps = dir.join("admin-steps");
    let out = devnet(
        &[
            "--network",
            "mainnet",
            "--chains",
            "config/chains.toml",
            "--deployments-dir",
            dir.join("deployments").to_str().unwrap(),
            "--vault-chain",
            "solana",
            "mainnet-solana-deploy",
            "--admin",
            &Keypair::new().pubkey().to_string(),
            "--programs-dir",
            programs.to_str().unwrap(),
            "--admin-steps-dir",
            steps.to_str().unwrap(),
            "--state-dir",
            dir.join("state").to_str().unwrap(),
        ],
        &[],
    );
    assert!(
        !out.status.success()
            && !steps.exists()
            && String::from_utf8_lossy(&out.stderr).contains("not the verifiable build"),
        "a deploy took programs that are not the verifiable build:\n{}",
        text(&out)
    );
}

/// A directory of this repository's `.shonin/mainnet-fork`, for one test,
/// removed when dropped.
struct ForkDir(PathBuf);

impl ForkDir {
    fn new(name: &str) -> Self {
        let dir = repo()
            .join(".shonin/mainnet-fork")
            .join(format!("test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for ForkDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The ids step, as the task runs it, with the program keypairs and the
/// mode's flags given.
fn ids_step(dir: &Path, keypairs: Option<&Path>, mode: &[&str], out: &Path) -> Output {
    let admin = Keypair::new().pubkey().to_string();
    let mut args = vec![
        "--network",
        "mainnet",
        "--deployments-dir",
        dir.join("deployments").to_str().unwrap(),
        "--vault-chain",
        "solana",
        "mainnet-solana-ids",
        "--admin",
        &admin,
        "--stand-ins",
        dir.join("stand-ins").to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
    ]
    .into_iter()
    .map(str::to_string)
    .collect::<Vec<_>>();
    if let Some(keypairs) = keypairs {
        args.extend(["--program-keypairs".into(), keypairs.display().to_string()]);
    }
    args.extend(mode.iter().map(|arg| arg.to_string()));
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    devnet(&args, &[])
}

/// A fork rehearsal takes the throwaway program keypairs the tool made
/// itself under `.shonin/mainnet-fork`, and nothing else there: the owner's
/// keypairs symlinked, hard-linked or copied into a directory under it are
/// refused, and no list of ids is written for the build. Before the first
/// real send no `mainnet-id` is committed, so nothing else would catch them.
#[test]
fn a_fork_rehearsal_takes_only_the_throwaway_keypairs_the_tool_made() {
    let dir = scratch("forkkeys");
    let owner = dir.join("owner-keys");
    keypair_dir(&owner);
    for how in ["linked", "hard-linked", "copied"] {
        let fork = ForkDir::new(how);
        for name in SHIPPING {
            let file = format!("{name}-keypair.json");
            let (from, to) = (owner.join(&file), fork.0.join(&file));
            match how {
                "linked" => std::os::unix::fs::symlink(&from, &to).unwrap(),
                "hard-linked" => std::fs::hard_link(&from, &to).unwrap(),
                _ => {
                    std::fs::copy(&from, &to).unwrap();
                }
            }
        }
        let out_file = dir.join(format!("ids-{how}"));
        let out = ids_step(&dir, Some(&fork.0), &["--on-fork"], &out_file);
        assert!(
            !out.status.success()
                && !out_file.exists()
                && String::from_utf8_lossy(&out.stderr).contains("never come onto a fork"),
            "a fork rehearsal took the owner's keypairs {how} into {}:\n{}",
            fork.0.display(),
            text(&out)
        );
    }
    // The tool's own throwaways, made by the ids step itself, are taken,
    // and taken again on the next run.
    let fork = ForkDir::new("made");
    let out_file = dir.join("ids-made");
    for _ in 0..2 {
        let out = ids_step(&dir, Some(&fork.0), &["--on-fork"], &out_file);
        assert!(out.status.success(), "{}", text(&out));
    }
    let listed = std::fs::read_to_string(&out_file).unwrap();
    for name in SHIPPING {
        let id = keys::read_keypair(&fork.0.join(format!("{name}-keypair.json")))
            .unwrap()
            .pubkey();
        assert!(listed.contains(&format!("{name} {id}")), "{listed}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A real send never takes a fork's throwaway program keypairs, however
/// they are reached: a directory outside `.shonin/mainnet-fork` whose files
/// are symlinks to them, or copies of them, is refused.
#[test]
fn a_real_send_never_takes_the_fork_s_throwaway_keypairs_by_a_link_or_a_copy() {
    let dir = scratch("sendkeys");
    let fork = ForkDir::new("send");
    let made = ids_step(&dir, Some(&fork.0), &["--on-fork"], &dir.join("fork-ids"));
    assert!(made.status.success(), "{}", text(&made));
    let deployer = dir.join("deployer.json");
    keys::write_keypair(&Keypair::new(), &deployer).unwrap();
    let send = [
        "--deployer-keyfile",
        deployer.to_str().unwrap(),
        "--confirm-mainnet",
        "mainnet-beta",
    ];
    for how in ["linked", "copied"] {
        let owner = dir.join(format!("owner-{how}"));
        std::fs::create_dir_all(&owner).unwrap();
        for name in SHIPPING {
            let file = format!("{name}-keypair.json");
            let (from, to) = (fork.0.join(&file), owner.join(&file));
            if how == "linked" {
                std::os::unix::fs::symlink(&from, &to).unwrap();
            } else {
                std::fs::copy(&from, &to).unwrap();
            }
        }
        let out_file = dir.join(format!("ids-{how}"));
        let out = ids_step(&dir, Some(&owner), &send, &out_file);
        assert!(
            !out.status.success()
                && !out_file.exists()
                && String::from_utf8_lossy(&out.stderr).contains("a real send takes the owner's"),
            "a real send took the fork's throwaways {how} from {}:\n{}",
            owner.display(),
            text(&out)
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Every mode reads each program's committed `mainnet-id` from this
/// repository's `contracts/solana/programs` alone: another directory (an
/// empty one would hide every committed id) is refused, on a fork too.
#[test]
fn the_committed_ids_are_read_from_this_repository_s_programs_in_every_mode() {
    let dir = scratch("sources");
    let empty = dir.join("programs");
    std::fs::create_dir_all(&empty).unwrap();
    let fork = ForkDir::new("sources");
    for mode in [&["--on-fork"][..], &[][..]] {
        let out_file = dir.join("ids");
        let keypairs = if mode.is_empty() {
            None
        } else {
            Some(fork.0.as_path())
        };
        let mut args = mode.to_vec();
        args.extend(["--program-sources", empty.to_str().unwrap()]);
        let out = ids_step(&dir, keypairs, &args, &out_file);
        assert!(
            !out.status.success()
                && !out_file.exists()
                && String::from_utf8_lossy(&out.stderr).contains("contracts/solana/programs alone"),
            "{mode:?} read the committed ids from {}:\n{}",
            empty.display(),
            text(&out)
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// The cluster the verifiable build's comparison reads is asked what it
/// is, never judged by the spelling of its address: without ON_FORK=1 a
/// local endpoint is refused in any spelling (an IPv6 loopback too), and a
/// Surfpool fork is refused wherever it is; with it, only a local Surfpool
/// fork of mainnet-beta is taken. What is printed is the endpoint checked,
/// alone.
#[test]
fn the_verifiable_build_s_comparison_reads_only_the_cluster_its_mode_names() {
    let port = 18741;
    fake_surfpool(port);
    let cluster = |args: &[&str]| {
        let mut all = vec![
            "--network",
            "mainnet",
            "--vault-chain",
            "solana",
            "mainnet-solana-cluster",
        ];
        all.extend(args);
        devnet(&all, &[])
    };
    let fork = format!("http://127.0.0.1:{port}");
    for spelling in [
        fork.clone(),
        format!("http://[::1]:{port}"),
        format!("http://localhost:{port}"),
    ] {
        let out = cluster(&["--rpc", &spelling]);
        assert!(
            !out.status.success(),
            "{spelling} was read as mainnet-beta:\n{}",
            text(&out)
        );
    }
    let out = cluster(&["--on-fork", "--rpc", &fork]);
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        fork,
        "{}",
        text(&out)
    );
    // A fork's check never takes a public endpoint.
    let out = cluster(&["--on-fork", "--rpc", "https://api.mainnet-beta.solana.com"]);
    assert!(!out.status.success(), "{}", text(&out));
}
