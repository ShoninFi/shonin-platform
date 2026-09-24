# The API

Everything the web app shows, it reads from this API. There is nothing it can see that you cannot.

The service behind it is the **indexer**, which is part of this repository and open source, so you can run your own against the same chain and compare. Running locally it listens on `http://localhost:19081`.

## Reading

### `GET /api/snapshot`

The entire state of the platform in one document: every agent with its vault, record, review, health and attestation; every author; the build queue; the activity feed; benchmarks and totals. It carries a version, so a client can poll cheaply and re-render only when it changes.

```bash
curl -s http://localhost:19081/api/snapshot | jq '.strategies[0] | {id, version, standing, vault: .vault.value}'
```

### `GET /api/config`

What a client needs to reach the chain itself, and the terms the platform is running under:

```json
{
  "chain":        { "chain": "local", "chain_id": 31337, "block": 42 },
  "rpc_url":      "http://localhost:19083",
  "addresses":    { "usd": "0x…", "venue": "0x…", "factory": "0x…", "…": "…" },
  "asset_symbol": "USDC",
  "asset_decimals": 6,
  "terms": {
    "minimum_tank":        40.0,
    "review_fee":          1.0,
    "fees":                { "streaming_bps": 50, "author_bps": 1000,
                             "platform_bps": 500, "transaction_bps": 10 },
    "exit_band_bps":       100,
    "freeze_after_days":   7.0,
    "image_timelock_days": 2.0,
    "tank_target_days":    30
  }
}
```

Every one of those terms is read from the contract that enforces it. The app shows them rather than numbers written into itself, so a page cannot go stale against the chain it is talking to. If the chain does not answer, the endpoint returns `503` rather than guessing.

### `GET /api/portfolio/{address}`

What one address holds: its balance outside vaults, its position in each vault with what it paid, and any claims waiting to settle. The cost basis is computed from each vault's own deposit and withdrawal events.

### `GET /api/jobs` and `GET /api/jobs/{id}`

The public build queue and one build, with its full log whether it succeeded or failed.

### `GET /api/store/{hash}/{name}`

The content-addressed store, addressed by the hashes the chain holds:

| Name | What |
| --- | --- |
| `source.tar` | The packed project, for a published agent |
| `bundle.tar` | The bundle: the component and any models |
| `component.wasm` | The compiled component |
| `manifest.json` | What the author declared |
| `log.json` | The build log |
| `hashes.json` | Every hash this build produced, beside the bytes they name |
| `report.json` | A review's full report |

Sealed source is kept so the reviewer can read it and a build can be repeated, and the store refuses to serve it.

## Writing

### `POST /api/submissions`

Hand a packed project to the build service. The vault must already exist, because nothing is compiled for free.

```json
{
  "strategy_id": "funding-basis",
  "version": 1,
  "author": "0x…",
  "package": "funding-basis",
  "sealed": false,
  "source": { "archive": { "base64": "…" } }
}
```

Replies with the job id and the source hash:

```json
{ "job_id": "8814", "source_hash": "sha256:…" }
```

### `POST /api/health`

Where an executor posts a vault's health report. It is signed by the key the registry blessed for that vault, over a canonical encoding of the report, and a report signed by any other key is refused. Anyone may read a health report; only the instance running the agent may write one.

## Also listening

Each service answers `GET /health` with its own name, and the build service serves its own queue directly. Those are operational endpoints, not part of the public surface.

## What is not here

The API never holds a key, never signs a transaction for you and never takes custody of anything. Depositing, leaving, settling and publishing are transactions your own wallet signs against the vault's own contract. The API is a convenience over the chain, and everything in it can be read from the chain without it.
