#!/usr/bin/env python3
"""Watches the testnets: every role's gas, the funders, the keeper and the API.

Reads public endpoints only, with no key. The accounts to watch come from the
JSON in the TESTNET_WATCH environment variable (testnet addresses are kept out
of the repository):

    {
      "evm": {"<role>": "0x…", …},          # the same address on every EVM chain
      "solana": {"<role>": "<base58>", …},
      "hypercore": {"<role>": "0x…", …}     # USDC held on Hyperliquid testnet's HyperCore
    }

Public endpoints keep no history, so each run adds a sample to a history file
(kept between runs by the workflow's cache) and judges from it:

- an account's burn rate is its fall since its last top-up, over at least
  MIN_SPAN_HOURS; the run fails when the balance would run out within
  RUNWAY_DAYS at that rate;
- the keeper role sends on every chain at least daily (its daily settlement),
  so the run fails when its transaction count on an EVM chain hasn't moved, or
  its newest Solana signature is older, than KEEPER_QUIET_HOURS;
- the API's /health must answer, with every chain read within API_STALE_SECONDS.

Usage: testnet-watch.py <history.json>. Exits 1 (and prints ::error::
annotations) when anything fails, so GitHub notifies the repository's owner.
"""

import json
import os
import sys
import time
import urllib.request

EVM_CHAINS = {
    "base-sepolia": "https://sepolia.base.org",
    "robinhood-testnet": "https://rpc.testnet.chain.robinhood.com",
    "hyperliquid-testnet": "https://rpc.hyperliquid-testnet.xyz/evm",
}
SOLANA_RPC = "https://api.devnet.solana.com"
HYPERCORE_INFO = "https://api.hyperliquid-testnet.xyz/info"
API_HEALTH = os.environ.get("API_HEALTH", "https://testnet-api.shonin.fi/health")

RUNWAY_DAYS = float(os.environ.get("RUNWAY_DAYS", "7"))
MIN_SPAN_HOURS = float(os.environ.get("MIN_SPAN_HOURS", "12"))
KEEPER_QUIET_HOURS = float(os.environ.get("KEEPER_QUIET_HOURS", "26"))
API_STALE_SECONDS = int(os.environ.get("API_STALE_SECONDS", "900"))
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


def check_gas(history, sample):
    now = sample["t"]
    for key, balance in sorted(sample["balances"].items()):
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

    history = [s for s in history if s["t"] > now - KEEP_DAYS * 86400] + [sample]
    with open(path, "w") as f:
        json.dump(history, f)

    sys.exit(1 if errors else 0)


if __name__ == "__main__":
    main()
