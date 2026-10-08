# Google Cloud

The images the machines and services run from, and how they are launched.
The tasks are in `taskfiles/cloud.yml`; the settings in `taskfiles/config.yml`.

## Two choices, both explicit

- **Where it runs:** here (`task dev`) or on Google Cloud (`task cloud:*`).
- **Which chains:** `NETWORK=testnet` or `NETWORK=mainnet`. The cloud never runs against `local`.

The project is chosen by the network, from `GCP_PROJECT_TESTNET` (`shonin-testnet`) or `GCP_PROJECT_MAINNET`. Every `gcloud` call passes `--project`, and `--zone` or `--region` where the resource has one, and a task refuses to run while its project is empty, so nothing ever lands in whatever project `gcloud` happens to default to.

## The VMs

Everything runs in one zone (`europe-west4-a`), on the default network, with nothing reachable from the internet: SSH only from Google's IAP range, the services' ports open to the VPC alone, and the API carried out to `https://testnet-api.shonin.fi` by a Cloudflare tunnel the services VM dials.

| VM | What runs | Machine | Account |
| --- | --- | --- | --- |
| `shonin-executor-<network>` | the executor, attested: Confidential Space, hardened image, Intel TDX | `c3-standard-4` | `shonin-workload`: attest, pull images, write logs; no secret |
| `shonin-builder-<network>` | the build service, attested the same way; on demand, off when idle | `c3-standard-4`, 30 GB disk | `shonin-workload` |
| `shonin-reviewer-<network>` | the reviewer, attested the same way; on demand, off when idle | `GCP_REVIEWER_MACHINE_TYPE` | `shonin-workload` |
| `shonin-services-<network>` | the API (the indexer inside), the keeper, the tunnel, the waker; Container-Optimized OS | `e2-small`, 10 GB standard disk | `shonin-services`: pull images, write logs, read three secrets, start, stop and read the prover's, the builder's and the reviewer's instances |
| `shonin-prover-<network>` | the attestation's prover; on-demand, off when idle | `GCP_PROVER_MACHINE_TYPE` | `shonin-prover`: pull images, write logs |

They name each other by internal DNS: the executor's status port is `http://shonin-executor-testnet.europe-west4-a.c.shonin-testnet.internal:19085`, the API `http://shonin-services-testnet.europe-west4-a.c.shonin-testnet.internal:19081`.

**How a blessing happens.** The executor makes its keys inside the enclave, asks the launcher for a token binding them, and publishes the token and its keys' leaves on its status port (`GET /attestation`), saying when a seal is wanted. The waker on the services VM sees that, and starts the prover's VM if it is off; the prover proves the token, posts the seal back to the executor, and powers off once idle. The keeper relays the seal to each chain's registry (`bless`, simulated first), then stocks each key the registry shows blessed for a live vault with gas.

**The root key.** The executor also holds the root key (`packages/rootkey`), on the network's canonical chain (`root` in `config/chains.toml`: Base Sepolia on the testnet). Finding no root recorded there, it makes one inside the enclave and adds its signer's leaf to the same tree, so the same seal records it: the keeper relays `setRootKey` to the canonical chain's registry alone, simulated first, once per seal. Once the registry records that signer, and while that record is live, the executor serves the root key service on its status port (`POST /rootkey/exchange`, `GET /rootkey/published/{purpose}`, `GET /rootkey`); the API relays the two reads (`/api/rootkey`, `/api/rootkey/published/{purpose}`). One executor holds the testnet's root, so replacing its VM loses it: the new executor finds another signer recorded and serves nothing derived (`GET /rootkey` says `holds_root: false`, and its log says why) until the admin clears the record (`task chains:clear-root-key NETWORK=testnet`), when it makes a new root. Everything sealed to the old root is sealed again (`task rootkey:seal`). `task chains:root-key NETWORK=testnet` prints the record.

**The builder and the reviewer, on demand.** Each gets its key from the executor by attested exchange: derived from the root and its image, so the same image under the same root has the same key, blessing and gas across boots. Each gets that key blessed on every chain as the executor does (its status port's `GET /attestation`, the prover, the keeper's relay), and the keeper stocks it with gas once blessed. Neither keeps anything on its own disk: its queue, payment cursor and unfinished work are sealed under its key and kept on the API's internal port (`:19082`, open to those two machines alone, never tunneled), every write signed by its blessed key; the bundles and reports it makes are pushed to the API's store, checked against their hashes, before anything on chain names them, and the executor reads them back from there (`/api/store`). Each exits once idle for ten minutes and its VM powers off (restart policy `Never`); the waker starts it again when the API says it is wanted: an upload reached a builder that was off (the upload is answered 503 with `Retry-After`, and the seed waits), a payment the builder has not read, the builder asking for the reviewer, a machine that stopped busy, or a blessing due for renewal. A new builder or reviewer image, or a new root, means a new key: `task cloud:drain NAME=builder|reviewer` waits until it holds nothing unfinished before either is replaced. The reviewer carries its model key sealed to the root, so every new root needs `task cloud:reviewer NETWORK=testnet` (reseal, rebuild, propose, drain and replace, revoke the old image).

**The root key's test requester** (`Dockerfile.requester`) proves sealing and release end to end: seal a dummy file to `platform:dummy` with `task rootkey:seal NETWORK=testnet PURPOSE=platform:dummy IN=<file> OUT=<dir>/sealed.json`, push the image with that directory baked in (`task cloud:push NAME=requester NETWORK=testnet SEALED=<dir>`), propose its digest as a reviewer (`task chains:image NETWORK=testnet DIGEST=<digest> KIND=reviewer`, then wait out the timelock), and launch it (`task cloud:deploy:machine NAME=requester NETWORK=testnet`). It asks the executor's status port for the key by attested exchange, opens the sealed file, and logs only the sha256 of what it opened. Delete its VM and revoke its image afterwards.

**Size.** The services VM is `e2-small` (`GCP_SERVICES_MACHINE_TYPE` in `taskfiles/config.yml`): the API, the keeper, cloudflared and the waker run beside Container-Optimized OS and Docker, and the indexer reads every chain every couple of seconds.

## The images

`Dockerfile.<name>` for each of: `executor`, `builder`, `reviewer`, `unwinder` (the machines), `api`, `keeper` (the services), `prover` (the prover's VM), `requester` (the root key's test), and `indexer` (not deployed: the API runs it). Each builds with the pinned toolchain and `--locked` from the repository root.

An image is built for one network, with everything it runs with baked in: `task cloud:build` passes the network (`NETWORK`, and `NETWORK_FEATURE` for the Solana program ids it compiles in: until the devnet ids are committed a testnet image has none, and serves the EVM chains alone, `SHONIN_WITHOUT_SOLANA=1`), the store's public copy (`STORE_URL`) and the API's store the executor reads (`STORE_READ_URL`), the internal URLs of the API (`API_URL`, and its internal port `API_INTERNAL_URL`), the executor (`EXECUTOR_URL`), the builder and the reviewer (`BUILDER_URL`, `REVIEWER_URL`), the reviewer's sealed model key as the build context `sealed`, and the generation's deployment files as the build context `deployments` (from `.shonin/<network>/deployments`, which the repository's own build context leaves out). The chains are the public endpoints committed in `config/chains.toml`: no keyed URL goes into an image. A redeploy is a rebuild.

The API's and the keeper's images hold no key. The keeper is handed its key alone where it is started.

```bash
task cloud:build NAME=api NETWORK=testnet      # docker build, tagged api:<git sha>
task cloud:push NAME=api NETWORK=testnet       # to europe-west4-docker.pkg.dev/shonin-testnet/shonin/api; prints the digest
```

### In Cloud Build

`BUILDER=cloud` builds in Google Cloud Build instead of cross-building here: `task cloud:push NAME=api NETWORK=testnet BUILDER=cloud`, or several side by side with `task cloud:images NETWORK=testnet NAMES="api keeper executor prover builder unwinder"` (Cloud Build is its default; the reviewer also takes `SEALED=<dir holding sealed.json>`). `cloudbuild/stage.sh` stages the source (the files git tracks or would track, never `.env`, `.shonin` or a target directory, and refusing outright anything that looks like one), the public deployment files alone as `_deployments/`, for the prover the guest as `_guest/`, and for the reviewer its sealed model key alone as `_sealed/sealed.json` (the key it was sealed from, `claude-api-key`, is gitignored and dockerignored, refused by name, and never staged); the tarball goes to `gs://shonin-testnet_cloudbuild/source/` and `cloudbuild.yaml` builds each image with BuildKit as the `shonin-cloudbuild` account. The Dockerfiles compile the dependencies in a stage of their own (cargo-chef), which Cloud Build pushes as an image named for everything it is made from (`<registry>/deps-<name>:<key>`: the Dockerfile, the toolchain, `Cargo.lock`, every `Cargo.toml`, the Solana crates named by path, the network's feature), so a build with the same inputs starts from it and compiles only the platform's own crates. The images are pushed as plain image manifests, as `docker push` would. `BUILDER=local` (the default for `cloud:push`) is the laptop build, kept as the fallback.

RISC Zero's Groth16 prover image is copied into the registry once (`task cloud:vendor-groth16 NETWORK=testnet`, `cloudbuild-vendor.yaml`), and the prover's VM pulls it from there by its digest (pinned in `prover/startup.sh`) and keeps it on its disk.

### One command

`task deploy:testnet SCOPE=services|contracts|all` (`deploy-testnet.sh`) runs every stage of a testnet deploy, each timed, safe to run again, and ends with the smoke check (`task chains:smoke NETWORK=testnet API=https://testnet-api.shonin.fi`, read-only, and the keeper's and the executor's health from inside the VPC).

## The machine: Confidential Space

Every machine is launched from its image's **digest**, because the digest is what the attestation measures, and with nothing passed to it: no `tee-env-`, no command override (the attestation refuses either; the image carries its settings and its arguments). The other launcher settings are not overrides: its lines go to Cloud Logging, and a container that stops is started again (the executor) or not, its VM then powering off (the builder, the reviewer, the unwinder).

```text
gcloud compute instances create shonin-executor-testnet \
  --project=shonin-testnet --zone=europe-west4-a \
  --confidential-compute-type=TDX --machine-type=c3-standard-4 --maintenance-policy=TERMINATE \
  --shielded-secure-boot \
  --image-project=confidential-space-images --image-family=confidential-space \
  --metadata="^~^tee-image-reference=europe-west4-docker.pkg.dev/shonin-testnet/shonin/executor@sha256:<digest>~tee-container-log-redirect=true~tee-restart-policy=Always" \
  --tags=shonin-executor \
  --service-account=shonin-workload@shonin-testnet.iam.gserviceaccount.com --scopes=cloud-platform
```

`task cloud:deploy:machine NAME=executor NETWORK=testnet` runs exactly that. Its logs are the launcher's (`logName="projects/shonin-testnet/logs/confidential-space-launcher"`), the container's lines in `jsonPayload.MESSAGE`: `task cloud:logs NAME=executor NETWORK=testnet`.

## The services VM

`services/startup.sh` runs at every boot. It reads the keeper's keys (its EVM key and its Solana devnet key) and the tunnel's token from Secret Manager with the VM's own token (never echoing them; each goes into an env file only root can read), logs in to Artifact Registry, and starts four containers on the host's network, each restarted by Docker if it stops and logging to Cloud Logging (`gcplogs-docker-driver`):

- `shonin-api`: the API on `0.0.0.0:19081`, reading through to `https://testnet-store.shonin.fi` for store files it doesn't hold, its store kept in `/var/lib/shonin/store`; uploads go to the builder, and its queue is kept beside the store (`/var/lib/shonin/api-state`) and shown while the builder is off. Its internal port `0.0.0.0:19082` keeps the builder's and the reviewer's sealed state and takes their built files.
- `shonin-keeper`: the keeper, health on `0.0.0.0:19087`, with `SHONIN_TESTNET_KEEPER_KEY` and `SHONIN_TESTNET_KEEPER_SOLANA_KEY` alone (never a mnemonic). Without the Solana key (a secret not made yet) it keeps the EVM chains, and its health says Solana devnet is not kept.
- `shonin-tunnel`: `cloudflared tunnel run`, its token from the env file.
- `shonin-waker`: `services/waker.sh`, every 30 seconds: when the executor, the builder or the reviewer wants a seal (`GET /attestation`) and the prover's instance is `TERMINATED`, it starts it (`POST .../instances/shonin-prover-testnet/start` with the VM's token), at most once every 15 minutes; when the API says the builder or the reviewer is wanted (`GET localhost:19082/internal/wake`) and it is `TERMINATED`, it starts it, at most once every 5 minutes; and it stops one still running after it exited or after 15 minutes without answering. `task cloud:test-waker` runs it against stand-ins.

What it runs is named in the instance's metadata (the images by digest), so `task cloud:redeploy:services` points it at new images and restarts it.

## The prover's VM

An on-demand VM, on only while proving, created by `task cloud:deploy:prover` (it boots once then, finds nothing to prove, and powers off when idle); its startup script (`prover/startup.sh`) runs the prover and powers the VM off when it exits. Its metadata names `shonin-prover-image` (by digest), `shonin-executor-url` and `shonin-status-urls` (the builder's and the reviewer's status ports). The services VM's account may start, stop and read this instance, the builder's and the reviewer's, and no other (a custom role, `shoninProverStarter`, bound on each instance).

## Standing it up (testnet)

```bash
task cloud:firewall NETWORK=testnet              # SSH from IAP only, no RDP, internal ports, nothing public
task cloud:secrets NETWORK=testnet               # the keeper's keys (derived here: EVM account 4, its Solana devnet key) and the tunnel's token, through stdin
task cloud:iam NETWORK=testnet                   # accounts, roles, secret access, the prover-starter role
for name in executor api keeper prover; do task cloud:push NAME=$name NETWORK=testnet; done
task cloud:deploy:machine NAME=executor NETWORK=testnet
task cloud:deploy:prover NETWORK=testnet         # also lets the services VM start it
task cloud:deploy:services NETWORK=testnet
# once the executor's root key is live:
task cloud:images NETWORK=testnet NAMES="builder unwinder"   # then propose each: task chains:image KIND=builder|unwinder
task cloud:deploy:machine NAME=builder NETWORK=testnet       # its first boot gets its key blessed, then it idles off
task cloud:reviewer NETWORK=testnet                          # seal the model key, build, propose, launch
task store:sync NETWORK=testnet                  # the published strategies' files to R2 (DRY_RUN=1 to list)
task cloud:status NETWORK=testnet
task cloud:logs NAME=api NETWORK=testnet         # executor | builder | reviewer | api | keeper | tunnel | waker | services | prover
```

Stopping and deleting: `task cloud:stop NAME=executor|services|prover NETWORK=testnet` (the disk stays and bills), `task cloud:start ...`, `task cloud:delete NAME=... NETWORK=testnet CONFIRM=yes`. Images: `gcloud artifacts docker images delete europe-west4-docker.pkg.dev/shonin-testnet/shonin/<name>@<digest> --project=shonin-testnet`.

The machines are never handed a mnemonic, nor any key: each makes its own inside the enclave and is blessed through attestation. The only keys in the cloud are the keeper's testnet key and the tunnel's token, readable by the services VM's account alone.
