#!/usr/bin/env python3
"""Watches the testnets: every role's gas, the funders, the keeper, the API
and the mirror feeds testnet-market keeps.

Reads public endpoints only, with no key. The accounts to watch come from the
JSON in the TESTNET_WATCH environment variable (testnet addresses are kept out
of the repository):

    {
      "evm": {"<role>": "0x…", …},          # the same address on every EVM chain
      "solana": {"<role>": "<base58>", …},
      "hypercore": {"<role>": "0x…", …},    # USDC held on Hyperliquid testnet's HyperCore
      "mirrors": {"<symbol>": "0x…", …}     # Robinhood Chain testnet's mirror feeds
    }

A role is listed only where it signs: the operator, which posts the stand-in
venue's prices, has no venue on a test network and is never funded there, so
it is not listed. testnet-market's account goes under "evm" (and "solana") as
"market", so its gas is watched like a role's.

Public endpoints keep no history, so each run adds a sample to a history file
(kept between runs by the workflow's cache) and judges from it:

- an account's burn rate is its fall since its last top-up, over at least
  MIN_SPAN_HOURS; the run fails when the balance would run out within
  RUNWAY_DAYS at that rate, or, for the keeper, when it holds less than its
  floor on that chain (keeper_balance_floor_micro in config/chains.toml): the
  keeper says the same at warn from inside, by the same rule;
- the keeper role sends on every chain at least daily (its daily settlement),
  so the run fails when its transaction count on an EVM chain hasn't moved, or
  its newest Solana signature is older, than KEEPER_QUIET_HOURS;
- the API's /health must answer, with every chain read within API_STALE_SECONDS;
- each mirror feed must carry its mainnet feed's latest round, once that round
  is older than MIRROR_LAG_SECONDS (testnet-market posts it within a minute or
  two).

Usage: testnet-watch.py <history.json>. Exits 1 (and prints ::error::
annotations) when anything fails, so GitHub notifies the repository's owner.
"""

import json
import os
import sys
import time
import tomllib
import urllib.request
from pathlib import Path

EVM_CHAINS = {
    "base-sepolia": "https://sepolia.base.org",
    "robinhood-testnet": "https://rpc.testnet.chain.robinhood.com",
    "hyperliquid-testnet": "https://rpc.hyperliquid-testnet.xyz/evm",
}
SOLANA_RPC = "https://api.devnet.solana.com"
# Each chain's key in config/chains.toml.
CHAIN_KEYS = {
    "base-sepolia": "evm:84532",
    "robinhood-testnet": "evm:46630",
    "hyperliquid-testnet": "evm:998",
    "solana-devnet": "solana:devnet",
}
CHAINS_TOML = Path(__file__).resolve().parents[2] / "config" / "chains.toml"
HYPERCORE_INFO = "https://api.hyperliquid-testnet.xyz/info"
API_HEALTH = os.environ.get("API_HEALTH", "https://testnet-api.shonin.fi/health")
# Where the mirrors' mainnet feeds are read.
MIRROR_CHAIN = "robinhood-testnet"
SOURCE_RPC = "https://rpc.mainnet.chain.robinhood.com"

RUNWAY_DAYS = float(os.environ.get("RUNWAY_DAYS", "7"))
MIN_SPAN_HOURS = float(os.environ.get("MIN_SPAN_HOURS", "12"))
KEEPER_QUIET_HOURS = float(os.environ.get("KEEPER_QUIET_HOURS", "26"))
API_STALE_SECONDS = int(os.environ.get("API_STALE_SECONDS", "900"))
MIRROR_LAG_SECONDS = int(os.environ.get("MIRROR_LAG_SECONDS", "1800"))
KEEP_DAYS = 8
# Some public endpoints refuse Python's default user agent.
HEADERS = {"content-type": "application/json", "user-agent": "shonin-testnet-watch"}

errors = []


def error(message):
    errors.append(message)
    print(f"::error::{message}")


def post(url, body, attempts=3):
    data = json.dumps(body).encode()
    for attempt in range(attempts):
        try:
            request = urllib.request.Request(url, data=data, headers=HEADERS)
            with urllib.request.urlopen(request, timeout=30) as response:
                return json.load(response)
        except Exception as failure:
            if attempt == attempts - 1:
                raise
            print(f"retrying {url}: {failure}")
            time.sleep(5 * (attempt + 1))


def rpc(url, method, params):
    answer = post(url, {"jsonrpc": "2.0", "id": 1, "method": method, "params": params})
    if "error" in answer:
        raise RuntimeError(f"{method}: {answer['error']}")
    return answer["result"]


def read(now, watch):
    """One sample: {key: value}, where a key names a chain and an account."""
    balances, nonces, keeper_last = {}, {}, {}

    for chain, url in EVM_CHAINS.items():
        for role, address in watch.get("evm", {}).items():
            key = f"{chain}/{role}"
            try:
                balances[key] = int(rpc(url, "eth_getBalance", [address, "latest"]), 16) / 1e18
                if role == "keeper":
                    nonces[chain] = int(
                        rpc(url, "eth_getTransactionCount", [address, "latest"]), 16
                    )
            except Exception as failure:
                print(f"::warning::{key}: unreadable this run ({failure})")

    for role, address in watch.get("solana", {}).items():
        key = f"solana-devnet/{role}"
        try:
            balances[key] = rpc(SOLANA_RPC, "getBalance", [address])["value"] / 1e9
            if role == "keeper":
                newest = rpc(SOLANA_RPC, "getSignaturesForAddress", [address, {"limit": 1}])
                keeper_last["solana-devnet"] = newest[0]["blockTime"] if newest else 0
        except Exception as failure:
            print(f"::warning::{key}: unreadable this run ({failure})")

    for role, address in watch.get("hypercore", {}).items():
        key = f"hypercore-testnet/{role}"
        try:
            state = post(HYPERCORE_INFO, {"type": "spotClearinghouseState", "user": address})
            balances[key] = sum(
                float(b["total"]) for b in state.get("balances", []) if b["coin"] == "USDC"
            )
        except Exception as failure:
            print(f"::warning::{key}: unreadable this run ({failure})")

    return {"t": now, "balances": balances, "nonces": nonces, "keeper_last": keeper_last}


def runway(history, key, now, balance):
    """Days left at the burn since the last top-up, or None if too little history."""
    oldest_t, oldest_value = now, balance
    newer = balance
    for sample in reversed(history):
        value = sample["balances"].get(key)
        if value is None:
            continue
        if value < newer:  # topped up after this sample: the window starts after it
            break
        oldest_t, oldest_value, newer = sample["t"], value, value
    span = now - oldest_t
    if span < MIN_SPAN_HOURS * 3600:
        return None, span
    burn = (oldest_value - balance) / span  # per second
    if burn <= 0:
        return float("inf"), span
    return balance / burn / 86400, span


def keeper_floors():
    """The keeper's balance floor on each chain, in whole coins, as the keeper reads it."""
    try:
        with open(CHAINS_TOML, "rb") as f:
            rpc = tomllib.load(f)["rpc"]
    except Exception as failure:
        print(f"::warning::the keeper's floors: unreadable ({failure})")
        return {}
    default = rpc["defaults"].get("keeper_balance_floor_micro", 0)
    return {
        chain: rpc["chains"].get(key, {}).get("keeper_balance_floor_micro", default) / 1e6
        for chain, key in CHAIN_KEYS.items()
    }


def check_gas(history, sample):
    now = sample["t"]
    floors = keeper_floors()
    for key, balance in sorted(sample["balances"].items()):
        chain, _, role = key.partition("/")
        floor = floors.get(chain, 0) if role == "keeper" else 0
        if balance < floor:
            error(f"{key} holds {balance:.6f}, under the keeper's floor of {floor:g} there: top it up")
        days, span = runway(history, key, now, balance)
        if days is None:
            print(f"{key}: {balance:.6f} (rate needs {MIN_SPAN_HOURS:.0f} h of history, has {span / 3600:.1f} h)")
        elif days == float("inf"):
            print(f"{key}: {balance:.6f}, not falling over {span / 3600:.1f} h")
        else:
            print(f"{key}: {balance:.6f}, about {days:.1f} days left at the rate of the last {span / 3600:.1f} h")
            if days < RUNWAY_DAYS:
                error(f"{key} runs out in about {days:.1f} days at its recent rate ({balance:.6f} left): top it up")


def check_keeper(history, sample):
    now = sample["t"]
    for chain, nonce in sorted(sample["nonces"].items()):
        since = now  # when the keeper's count last moved, as far as the history shows
        for older in reversed(history):
            if older["nonces"].get(chain) is None:
                continue
            if older["nonces"][chain] != nonce:
                break
            since = older["t"]
        quiet = (now - since) / 3600
        print(f"{chain}/keeper: {nonce} transactions, unchanged for at least {quiet:.1f} h")
        if quiet > KEEPER_QUIET_HOURS:
            error(f"the keeper has sent nothing on {chain} for {quiet:.0f} hours")
    for chain, last in sorted(sample["keeper_last"].items()):
        quiet = (now - last) / 3600
        print(f"{chain}/keeper: newest transaction {quiet:.1f} h ago")
        if quiet > KEEPER_QUIET_HOURS:
            error(f"the keeper has sent nothing on {chain} for {quiet:.0f} hours")


def check_api():
    try:
        request = urllib.request.Request(API_HEALTH, headers=HEADERS)
        with urllib.request.urlopen(request, timeout=60) as response:
            body = response.read().decode()
    except Exception as failure:
        error(f"{API_HEALTH} doesn't answer: {failure}")
        return
    print(body.strip())
    for line in body.splitlines():
        # "base: read 10 s ago"
        name, _, rest = line.partition(": read ")
        if rest.endswith(" s ago") and int(rest.split()[0]) > API_STALE_SECONDS:
            error(f"the API last read {name} {rest}")


# latestRoundData() and source(): their selectors.
LATEST_ROUND = "0xfeaf968c"
SOURCE = "0x67e828bf"


def latest_round(url, feed):
    """A feed's latest round id and its updatedAt."""
    words = rpc(url, "eth_call", [{"to": feed, "data": LATEST_ROUND}, "latest"])[2:]
    word = lambda i: int(words[64 * i : 64 * (i + 1)], 16)
    return word(0), word(3)


def check_mirrors(watch):
    now = int(time.time())
    url = EVM_CHAINS[MIRROR_CHAIN]
    for symbol, mirror in sorted(watch.get("mirrors", {}).items()):
        try:
            source = "0x" + rpc(url, "eth_call", [{"to": mirror, "data": SOURCE}, "latest"])[-40:]
            mirror_round, mirror_at = latest_round(url, mirror)
            source_round, source_at = latest_round(SOURCE_RPC, source)
        except Exception as failure:
            print(f"::warning::{symbol}'s mirror: unreadable this run ({failure})")
            continue
        behind = source_round != mirror_round
        print(
            f"{symbol}: mirror round {mirror_round} ({(now - mirror_at) / 3600:.1f} h old), "
            f"mainnet {source_round}{' (newer)' if behind else ''}"
        )
        if behind and now - source_at > MIRROR_LAG_SECONDS:
            error(
                f"{symbol}'s mirror lacks the mainnet round of {(now - source_at) / 60:.0f} minutes ago: "
                "is testnet-market running?"
            )


def main():
    path = sys.argv[1]
    watch = json.loads(os.environ["TESTNET_WATCH"])
    try:
        with open(path) as f:
            history = json.load(f)
    except (FileNotFoundError, ValueError):
        history = []

    now = int(time.time())
    sample = read(now, watch)
    print("== gas")
    check_gas(history, sample)
    print("== keeper")
    check_keeper(history, sample)
    print("== api")
    check_api()
    print("== mirror feeds")
    check_mirrors(watch)

    history = [s for s in history if s["t"] > now - KEEP_DAYS * 86400] + [sample]
    with open(path, "w") as f:
        json.dump(history, f)

    sys.exit(1 if errors else 0)


if __name__ == "__main__":
    main()
