# Google Cloud

The images the machines and services run from, and how they are launched.
The tasks are in `taskfiles/cloud.yml`; the settings in `taskfiles/config.yml`.

## Two choices, both explicit

- **Where it runs:** here (`task dev`) or on Google Cloud (`task cloud:*`).
- **Which chains:** `NETWORK=testnet` or `NETWORK=mainnet`. The cloud never runs against `local`.

The project is chosen by the network, from `GCP_PROJECT_TESTNET` or `GCP_PROJECT_MAINNET`. Every `gcloud` call passes `--project` and `--region` or `--zone`, and a task refuses to run while its project is empty, so nothing ever lands in whatever project `gcloud` happens to default to.

## The images

`Dockerfile.<name>` for each of: `reviewer`, `builder`, `executor`, `unwinder` (the machines), and `api`, `indexer`, `keeper` (the services). Each builds with the pinned toolchain and `--locked` from the repository root, and carries `config/chains.toml` and the committed mainnet deployments under `/etc/shonin`.

- The builder's runtime keeps the Rust toolchain and the strategy SDK, because it compiles strategies.
- The reviewer's image does not carry the CLI its model is reached through yet; without it every strategy shows "review pending".

```bash
task cloud:build NAME=executor                     # docker build, tagged executor:<git sha>
task cloud:push NAME=executor NETWORK=testnet      # to <region>-docker.pkg.dev/<project>/<repo>/executor
task cloud:deploy:machine NAME=executor NETWORK=testnet
task cloud:deploy:service NAME=api NETWORK=testnet
task cloud:status NETWORK=testnet
task cloud:logs NAME=executor NETWORK=testnet
```

## Machines run on Confidential Space

A machine is launched on an Intel TDX VM from the `confidential-space` image family, with its container named by **digest**, because the digest is what the attestation measures:

```text
gcloud compute instances create shonin-<name>-<network> \
  --project=<project> --zone=<zone> \
  --confidential-compute-type=TDX --machine-type=c3-standard-4 --maintenance-policy=TERMINATE \
  --shielded-secure-boot \
  --image-project=confidential-space-images --image-family=confidential-space \
  --metadata="^~^tee-image-reference=<region>-docker.pkg.dev/<project>/<repo>/<name>@sha256:<digest>~tee-container-log-redirect=true~tee-restart-policy=Always" \
  --service-account=<account>@<project>.iam.gserviceaccount.com --scopes=cloud-platform
```

`confidential-space-debug` is the debug image family, `SEV` the AMD alternative to `TDX`, and `tee-env-NAME=value` passes an environment variable to the container. The services run on Cloud Run, from the same registry.

Testnet addresses are never committed, so a testnet image carries none. A testnet launch hands every chain's deployment over whole, as a JSON list in `SHONIN_DEPLOYMENTS_JSON` (a `tee-env-` value on a machine, an environment variable on Cloud Run), read from `.shonin/testnet/deployments/`, so run `task chains:deploy NETWORK=testnet` first.

The testnet services sign with the role accounts from `SHONIN_TESTNET_EVM_MNEMONIC`, which they read from Secret Manager: `task cloud:secrets NETWORK=testnet` uploads it from `.env` (secret `GCP_SECRET_TESTNET_EVM_MNEMONIC`). The machines are never handed a mnemonic: each makes its own key inside the enclave and is blessed through attestation.

Nothing is deployed yet: the project ids are empty until the projects exist.
