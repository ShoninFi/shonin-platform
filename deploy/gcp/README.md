# Google Cloud

The images the machines and services run from, and how they are launched.
The tasks are in `taskfiles/cloud.yml`; the settings in `taskfiles/config.yml`.

## Two choices, both explicit

- **Where it runs:** here (`task dev`) or on Google Cloud (`task cloud:*`).
- **Which chains:** `NETWORK=testnet` or `NETWORK=mainnet`. The cloud never runs against `local`.

The project is chosen by the network, from `GCP_PROJECT_TESTNET` (`shonin-testnet`) or `GCP_PROJECT_MAINNET`. Every `gcloud` call passes `--project`, and `--zone` or `--region` where the resource has one, and a task refuses to run while its project is empty, so nothing ever lands in whatever project `gcloud` happens to default to.

## Three VMs

Everything runs in one zone (`us-central1-a`), on the default network, with nothing reachable from the internet: SSH only from Google's IAP range, the services' ports open to the VPC alone, and the API carried out to `https://testnet-api.shonin.fi` by a Cloudflare tunnel the services VM dials.

| VM | What runs | Machine | Account |
| --- | --- | --- | --- |
| `shonin-executor-<network>` | the executor, attested: Confidential Space, hardened image, Intel TDX | `c3-standard-4` | `shonin-workload`: attest, pull images, write logs; no secret |
| `shonin-services-<network>` | the API (the indexer inside), the keeper, the tunnel, the waker; Container-Optimized OS | `e2-small`, 10 GB standard disk | `shonin-services`: pull images, write logs, read two secrets, start and read the prover's instance |
| `shonin-prover-<network>` | the attestation's prover; Spot, off when idle | `GCP_PROVER_MACHINE_TYPE` | `shonin-prover`: pull images, write logs |

They name each other by internal DNS: the executor's status port is `http://shonin-executor-testnet.us-central1-a.c.shonin-testnet.internal:19085`, the API `http://shonin-services-testnet.us-central1-a.c.shonin-testnet.internal:19081`.

**How a blessing happens.** The executor makes its keys inside the enclave, asks the launcher for a token binding them, and publishes the token and its keys' leaves on its status port (`GET /attestation`), saying when a seal is wanted. The waker on the services VM sees that, and starts the prover's VM if it is off; the prover proves the token, posts the seal back to the executor, and powers off once idle. The keeper relays the seal to each chain's registry (`bless`, simulated first), then stocks each key the registry shows blessed for a live vault with gas.

**Memory.** The API, the keeper, cloudflared and the waker together should take a few hundred megabytes beside the OS and Docker (not yet measured), so `e2-micro` (1 GB, free in `us-central1`) may hold them; but its CPU is a quarter of a core sustained, and the indexer reads every chain every couple of seconds. The VM starts as `e2-small` (`GCP_SERVICES_MACHINE_TYPE`); move it to `e2-micro` only after `docker stats` on the VM shows the containers' memory and CPU fit with room.

## The images

`Dockerfile.<name>` for each of: `executor` (the machine; `reviewer`, `builder`, `unwinder` are built the same way), `api`, `keeper` (the services), `prover` (the prover's VM), and `indexer` (not deployed: the API runs it). Each builds with the pinned toolchain and `--locked` from the repository root.

An image is built for one network, with everything it runs with baked in: `task cloud:build` passes the network (`NETWORK`, and `NETWORK_FEATURE` for the Solana program ids it compiles in: until the devnet ids are committed a testnet image has none, and serves the EVM chains alone, `SHONIN_WITHOUT_SOLANA=1`), the store's public copy (`STORE_URL`), the API's and the executor's internal URLs (`API_URL`, `EXECUTOR_URL`), and the generation's deployment files as the build context `deployments` (from `.shonin/<network>/deployments`, which the repository's own build context leaves out). The chains are the public endpoints committed in `config/chains.toml`: no keyed URL goes into an image. A redeploy is a rebuild.

The API's and the keeper's images hold no key. The keeper is handed its key alone where it is started.

```bash
task cloud:build NAME=api NETWORK=testnet      # docker build, tagged api:<git sha>
task cloud:push NAME=api NETWORK=testnet       # to us-central1-docker.pkg.dev/shonin-testnet/shonin/api; prints the digest
```

## The machine: Confidential Space

The executor is launched from its image's **digest**, because the digest is what the attestation measures, and with nothing passed to it: no `tee-env-`, no command override (the attestation refuses either; the image carries its settings and its arguments). The other launcher settings are not overrides: its lines go to Cloud Logging, and a container that stops is started again.

```text
gcloud compute instances create shonin-executor-testnet \
  --project=shonin-testnet --zone=us-central1-a \
  --confidential-compute-type=TDX --machine-type=c3-standard-4 --maintenance-policy=TERMINATE \
  --shielded-secure-boot \
  --image-project=confidential-space-images --image-family=confidential-space \
  --metadata="^~^tee-image-reference=us-central1-docker.pkg.dev/shonin-testnet/shonin/executor@sha256:<digest>~tee-container-log-redirect=true~tee-restart-policy=Always" \
  --tags=shonin-executor \
  --service-account=shonin-workload@shonin-testnet.iam.gserviceaccount.com --scopes=cloud-platform
```

`task cloud:deploy:machine NAME=executor NETWORK=testnet` runs exactly that. Its logs are the launcher's (`logName="projects/shonin-testnet/logs/confidential-space-launcher"`), the container's lines in `jsonPayload.MESSAGE`: `task cloud:logs NAME=executor NETWORK=testnet`.

## The services VM

`services/startup.sh` runs at every boot. It reads the keeper's key and the tunnel's token from Secret Manager with the VM's own token (never echoing them; each goes into an env file only root can read), logs in to Artifact Registry, and starts four containers on the host's network, each restarted by Docker if it stops and logging to Cloud Logging (`gcplogs-docker-driver`):

- `shonin-api`: the API on `0.0.0.0:19081`, reading through to `https://testnet-store.shonin.fi` for store files it doesn't hold, its store kept in `/var/lib/shonin/store`. No build service is attached, so the snapshot has no queue and uploads are refused.
- `shonin-keeper`: the keeper, health on `0.0.0.0:19087`, with `SHONIN_TESTNET_KEEPER_KEY` alone.
- `shonin-tunnel`: `cloudflared tunnel run`, its token from the env file.
- `shonin-waker`: `services/waker.sh`, polling the executor's `GET /attestation` every 30 seconds; when a seal is wanted and the prover's instance is `TERMINATED`, it starts it (`POST .../instances/shonin-prover-testnet/start` with the VM's token), at most once every 15 minutes.

What it runs is named in the instance's metadata (the images by digest), so `task cloud:redeploy:services` points it at new images and restarts it.

## The prover's VM

A Spot VM, stopped (not deleted) when reclaimed, created by `task cloud:deploy:prover` (it boots once then, finds nothing to prove, and powers off when idle); its startup script (`prover/startup.sh`) runs the prover and powers the VM off when it exits. Its metadata names `shonin-prover-image` (by digest) and `shonin-executor-url`. The services VM's account may start and read this instance and no other (a custom role, `shoninProverStarter`, bound on the instance).

## Standing it up (testnet)

```bash
task cloud:firewall NETWORK=testnet              # SSH from IAP only, no RDP, internal ports, nothing public
task cloud:secrets NETWORK=testnet               # the keeper's key (derived here, account 4) and the tunnel's token, through stdin
task cloud:iam NETWORK=testnet                   # accounts, roles, secret access, the prover-starter role
for name in executor api keeper prover; do task cloud:push NAME=$name NETWORK=testnet; done
task cloud:deploy:machine NAME=executor NETWORK=testnet
task cloud:deploy:prover NETWORK=testnet         # also lets the services VM start it
task cloud:deploy:services NETWORK=testnet
task store:sync NETWORK=testnet                  # the published strategies' files to R2 (DRY_RUN=1 to list)
task cloud:status NETWORK=testnet
task cloud:logs NAME=api NETWORK=testnet         # executor | api | keeper | tunnel | waker | services | prover
```

Stopping and deleting: `task cloud:stop NAME=executor|services|prover NETWORK=testnet` (the disk stays and bills), `task cloud:start ...`, `task cloud:delete NAME=... NETWORK=testnet CONFIRM=yes`. Images: `gcloud artifacts docker images delete us-central1-docker.pkg.dev/shonin-testnet/shonin/<name>@<digest> --project=shonin-testnet`.

The machines are never handed a mnemonic, nor any key: each makes its own inside the enclave and is blessed through attestation. The only keys in the cloud are the keeper's testnet key and the tunnel's token, readable by the services VM's account alone.
