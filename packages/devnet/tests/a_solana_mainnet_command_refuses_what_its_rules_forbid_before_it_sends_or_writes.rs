//! The Solana mainnet commands refuse what their rules forbid before they
//! send or write anything. Each test runs the devnet binary; the one that
//! needs a cluster talks to a fake JSON-RPC node on a loopback port (18740)
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
    let sources = dir.join("programs");
    std::fs::create_dir_all(&sources).unwrap();
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
            "--program-sources",
            sources.to_str().unwrap(),
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

/// Once the mainnet ids are committed, a dry run without
/// MAINNET_PROGRAM_KEYPAIRS lists the committed ids for the build (which
/// refuses any other), not stand-ins.
#[test]
fn a_dry_run_after_the_deploy_builds_with_the_committed_ids() {
    let dir = scratch("dryrun");
    let sources = dir.join("programs");
    let mut committed = Vec::new();
    for name in SHIPPING {
        let id = Keypair::new().pubkey();
        let program = sources.join(name.replace('_', "-"));
        std::fs::create_dir_all(&program).unwrap();
        std::fs::write(program.join("mainnet-id"), format!("{id}\n")).unwrap();
        committed.push((name, id));
    }
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
            "--program-sources",
            sources.to_str().unwrap(),
            "--stand-ins",
            dir.join("stand-ins").to_str().unwrap(),
            "--out",
            out_file.to_str().unwrap(),
        ],
        &[],
    );
    assert!(out.status.success(), "{}", text(&out));
    let listed = std::fs::read_to_string(&out_file).unwrap_or_default();
    for (name, id) in committed {
        assert!(
            listed.contains(&format!("{name} {id}")),
            "the dry run listed a stand-in for {name} beside its committed id {id}; the build \
             it feeds refuses that:\n{listed}\n{}",
            text(&out)
        );
    }
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
