# Google Cloud

The images the machines and services run from, and how they are launched.
The tasks are in `taskfiles/cloud.yml`; the settings in `taskfiles/config.yml`.

## Two choices, both explicit

- **Where it runs:** here (`task dev`) or on Google Cloud (`task cloud:*`).
- **Which chains:** `NETWORK=testnet` or `NETWORK=mainnet`. The cloud never runs against `local`.

The project is chosen by the network, from `GCP_PROJECT_TESTNET` (`shonin-testnet`) or `GCP_PROJECT_MAINNET`. Every `gcloud` call passes `--project`, and `--zone` or `--region` where the resource has one, and a task refuses to run while its project is empty, so nothing ever lands in whatever project `gcloud` happens to default to.

## Three VMs

Everything runs in one zone (`europe-west4-a`), on the default network, with nothing reachable from the internet: SSH only from Google's IAP range, the services' ports open to the VPC alone, and the API carried out to `https://testnet-api.shonin.fi` by a Cloudflare tunnel the services VM dials.

| VM | What runs | Machine | Account |
| --- | --- | --- | --- |
| `shonin-executor-<network>` | the executor, attested: Confidential Space, hardened image, Intel TDX | `c3-standard-4` | `shonin-workload`: attest, pull images, write logs; no secret |
| `shonin-services-<network>` | the API (the indexer inside), the keeper, the tunnel, the waker; Container-Optimized OS | `e2-small`, 10 GB standard disk | `shonin-services`: pull images, write logs, read two secrets, start and read the prover's instance |
| `shonin-prover-<network>` | the attestation's prover; on-demand, off when idle | `GCP_PROVER_MACHINE_TYPE` | `shonin-prover`: pull images, write logs |

They name each other by internal DNS: the executor's status port is `http://shonin-executor-testnet.europe-west4-a.c.shonin-testnet.internal:19085`, the API `http://shonin-services-testnet.europe-west4-a.c.shonin-testnet.internal:19081`.

**How a blessing happens.** The executor makes its keys inside the enclave, asks the launcher for a token binding them, and publishes the token and its keys' leaves on its status port (`GET /attestation`), saying when a seal is wanted. The waker on the services VM sees that, and starts the prover's VM if it is off; the prover proves the token, posts the seal back to the executor, and powers off once idle. The keeper relays the seal to each chain's registry (`bless`, simulated first), then stocks each key the registry shows blessed for a live vault with gas.

**The root key.** The executor also holds the root key (`packages/rootkey`), on the network's canonical chain (`root` in `config/chains.toml`: Base Sepolia on the testnet). Finding no root recorded there, it makes one inside the enclave and adds its signer's leaf to the same tree, so the same seal records it: the keeper relays `setRootKey` to the canonical chain's registry alone, simulated first, once per seal. Once the registry records that signer, and while that record is live, the executor serves the root key service on its status port (`POST /rootkey/exchange`, `GET /rootkey/published/{purpose}`, `GET /rootkey`); the API relays the two reads (`/api/rootkey`, `/api/rootkey/published/{purpose}`). One executor holds the testnet's root, so replacing its VM loses it: the new executor finds another signer recorded and serves nothing derived (`GET /rootkey` says `holds_root: false`, and its log says why) until the admin clears the record (`task chains:clear-root-key NETWORK=testnet`), when it makes a new root. Everything sealed to the old root is sealed again (`task rootkey:seal`). `task chains:root-key NETWORK=testnet` prints the record.

**The root key's test requester** (`Dockerfile.requester`) proves sealing and release end to end: seal a dummy file to `platform:dummy` with `task rootkey:seal NETWORK=testnet PURPOSE=platform:dummy IN=<file> OUT=<dir>/sealed.json`, push the image with that directory baked in (`task cloud:push NAME=requester NETWORK=testnet SEALED=<dir>`), propose its digest as a reviewer (`task chains:image NETWORK=testnet DIGEST=<digest> KIND=reviewer`, then wait out the timelock), and launch it (`task cloud:deploy:machine NAME=requester NETWORK=testnet`). It asks the executor's status port for the key by attested exchange, opens the sealed file, and logs only the sha256 of what it opened. Delete its VM and revoke its image afterwards.

**Size.** The services VM is `e2-small` (`GCP_SERVICES_MACHINE_TYPE` in `taskfiles/config.yml`): the API, the keeper, cloudflared and the waker run beside Container-Optimized OS and Docker, and the indexer reads every chain every couple of seconds.

## The images

`Dockerfile.<name>` for each of: `executor` (the machine; `reviewer`, `builder`, `unwinder` are built the same way), `api`, `keeper` (the services), `prover` (the prover's VM), and `indexer` (not deployed: the API runs it). Each builds with the pinned toolchain and `--locked` from the repository root.

An image is built for one network, with everything it runs with baked in: `task cloud:build` passes the network (`NETWORK`, and `NETWORK_FEATURE` for the Solana program ids it compiles in: until the devnet ids are committed a testnet image has none, and serves the EVM chains alone, `SHONIN_WITHOUT_SOLANA=1`), the store's public copy (`STORE_URL`), the API's and the executor's internal URLs (`API_URL`, `EXECUTOR_URL`), and the generation's deployment files as the build context `deployments` (from `.shonin/<network>/deployments`, which the repository's own build context leaves out). The chains are the public endpoints committed in `config/chains.toml`: no keyed URL goes into an image. A redeploy is a rebuild.

The API's and the keeper's images hold no key. The keeper is handed its key alone where it is started.

```bash
task cloud:build NAME=api NETWORK=testnet      # docker build, tagged api:<git sha>
task cloud:push NAME=api NETWORK=testnet       # to europe-west4-docker.pkg.dev/shonin-testnet/shonin/api; prints the digest
```

### In Cloud Build

`BUILDER=cloud` builds in Google Cloud Build instead of cross-building here: `task cloud:push NAME=api NETWORK=testnet BUILDER=cloud`, or several side by side with `task cloud:images NETWORK=testnet NAMES="api keeper executor prover"` (Cloud Build is its default). `cloudbuild/stage.sh` stages the source (the files git tracks or would track, never `.env`, `.shonin` or a target directory, and refusing outright anything that looks like one), the public deployment files alone as `_deployments/`, and for the prover the guest as `_guest/`; the tarball goes to `gs://shonin-testnet_cloudbuild/source/` and `cloudbuild.yaml` builds each image with BuildKit as the `shonin-cloudbuild` account. The Dockerfiles compile the dependencies in a stage of their own (cargo-chef), which Cloud Build pushes as an image named for everything it is made from (`<registry>/deps-<name>:<key>`: the Dockerfile, the toolchain, `Cargo.lock`, every `Cargo.toml`, the Solana crates named by path, the network's feature), so a build with the same inputs starts from it and compiles only the platform's own crates. The images are pushed as plain image manifests, as `docker push` would. `BUILDER=local` (the default for `cloud:push`) is the laptop build, kept as the fallback.

RISC Zero's Groth16 prover image is copied into the registry once (`task cloud:vendor-groth16 NETWORK=testnet`, `cloudbuild-vendor.yaml`), and the prover's VM pulls it from there by its digest (pinned in `prover/startup.sh`) and keeps it on its disk.

### One command

`task deploy:testnet SCOPE=services|contracts|all` (`deploy-testnet.sh`) runs every stage of a testnet deploy, each timed, safe to run again, and ends with the smoke check (`task chains:smoke NETWORK=testnet API=https://testnet-api.shonin.fi`, read-only, and the keeper's and the executor's health from inside the VPC).

## The machine: Confidential Space

The executor is launched from its image's **digest**, because the digest is what the attestation measures, and with nothing passed to it: no `tee-env-`, no command override (the attestation refuses either; the image carries its settings and its arguments). The other launcher settings are not overrides: its lines go to Cloud Logging, and a container that stops is started again.

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

`services/startup.sh` runs at every boot. It reads the keeper's key and the tunnel's token from Secret Manager with the VM's own token (never echoing them; each goes into an env file only root can read), logs in to Artifact Registry, and starts four containers on the host's network, each restarted by Docker if it stops and logging to Cloud Logging (`gcplogs-docker-driver`):

- `shonin-api`: the API on `0.0.0.0:19081`, reading through to `https://testnet-store.shonin.fi` for store files it doesn't hold, its store kept in `/var/lib/shonin/store`. No build service is attached, so the snapshot has no queue and uploads are refused.
- `shonin-keeper`: the keeper, health on `0.0.0.0:19087`, with `SHONIN_TESTNET_KEEPER_KEY` alone.
- `shonin-tunnel`: `cloudflared tunnel run`, its token from the env file.
- `shonin-waker`: `services/waker.sh`, polling the executor's `GET /attestation` every 30 seconds; when a seal is wanted and the prover's instance is `TERMINATED`, it starts it (`POST .../instances/shonin-prover-testnet/start` with the VM's token), at most once every 15 minutes.

What it runs is named in the instance's metadata (the images by digest), so `task cloud:redeploy:services` points it at new images and restarts it.

## The prover's VM

An on-demand VM, on only while proving, created by `task cloud:deploy:prover` (it boots once then, finds nothing to prove, and powers off when idle); its startup script (`prover/startup.sh`) runs the prover and powers the VM off when it exits. Its metadata names `shonin-prover-image` (by digest) and `shonin-executor-url`. The services VM's account may start and read this instance and no other (a custom role, `shoninProverStarter`, bound on the instance).

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

Stopping and deleting: `task cloud:stop NAME=executor|services|prover NETWORK=testnet` (the disk stays and bills), `task cloud:start ...`, `task cloud:delete NAME=... NETWORK=testnet CONFIRM=yes`. Images: `gcloud artifacts docker images delete europe-west4-docker.pkg.dev/shonin-testnet/shonin/<name>@<digest> --project=shonin-testnet`.

The machines are never handed a mnemonic, nor any key: each makes its own inside the enclave and is blessed through attestation. The only keys in the cloud are the keeper's testnet key and the tunnel's token, readable by the services VM's account alone.
