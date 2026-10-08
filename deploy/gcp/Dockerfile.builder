# syntax=docker/dockerfile:1
# The builder: reviews, then compiles, submitted agents into signed bundles.
# Runs on a Confidential Space VM, which attests to this image by its digest,
# started on demand by the services VM's waker and powered off when idle.
# Built from the repository root by `task cloud:images NAMES=builder NETWORK=testnet`
# (or `task cloud:build NAME=builder NETWORK=testnet`), which passes the
# network's settings and its deployments directory:
#   docker build --build-arg NETWORK_FEATURE=none --build-arg WITHOUT_SOLANA=1 --build-arg NETWORK=testnet \
#     --build-arg EXECUTOR_URL=... --build-arg REVIEWER_URL=... --build-arg API_INTERNAL_URL=... \
#     --build-context deployments=.shonin/testnet/deployments \
#     -f deploy/gcp/Dockerfile.builder .
#
# Everything it runs with is in the image, its arguments included: the
# attestation refuses a launch that overrides the image's command or
# environment. It holds no key from outside: its own key is derived for this
# image by the key holder (the executor) and released to it by attested
# exchange, and what it keeps off the machine is sealed under that key.

# Where the build starts from: the `deps` stage below, or (in Cloud Build)
# that stage already built and pushed as an image named for everything it
# is made from (deploy/gcp/cloudbuild/build.sh).
ARG DEPS=deps

# The toolchain rust-toolchain.toml pins, with its targets, and cargo-chef.
FROM rust:1.98.1-slim-bookworm AS chef
WORKDIR /src
COPY rust-toolchain.toml ./
RUN rustup toolchain install && rustup show active-toolchain && \
    cargo install cargo-chef --locked --version 0.1.78

# What the dependencies are, from the manifests alone.
FROM chef AS recipe
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

# The dependencies alone, compiled from the recipe.
FROM chef AS deps
# The network whose Solana program ids are compiled in: devnet for a testnet
# image, mainnet for a mainnet one, or none for an image that serves no
# Solana cluster (task cloud:build passes it, with WITHOUT_SOLANA=1 for none).
ARG NETWORK_FEATURE
# Thin LTO over 16 codegen units links in a fraction of the time of the
# workspace's fat, single-unit release, and the build stays reproducible.
ENV CARGO_PROFILE_RELEASE_LTO=thin \
    CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16
COPY --from=recipe /src/recipe.json recipe.json
# The Solana programs' crates the chain crate names by path: outside the
# workspace, so the recipe carries no skeleton of them.
COPY contracts/solana contracts/solana
RUN : "${NETWORK_FEATURE:?pass --build-arg NETWORK_FEATURE=devnet, mainnet or none}" && \
    features="" && { [ "$NETWORK_FEATURE" = none ] || features="--features $NETWORK_FEATURE"; } && \
    cargo chef cook --release --locked -p shonin-builder --bin builder $features --recipe-path recipe.json

# Then the platform itself. rust-toolchain.toml pins the same compiler;
# --locked builds exactly Cargo.lock.
FROM ${DEPS} AS build
ARG NETWORK_FEATURE
COPY . .
RUN : "${NETWORK_FEATURE:?pass --build-arg NETWORK_FEATURE=devnet, mainnet or none}" && \
    features="" && { [ "$NETWORK_FEATURE" = none ] || features="--features $NETWORK_FEATURE"; } && \
    cargo build --release --locked -p shonin-builder --bin builder $features && \
    mkdir -p /out && \
    target/release/builder --vendor-into /out/vendor && \
    cargo package -p shonin-strategy-sdk --no-verify --allow-dirty && \
    mkdir -p /out/strategy-sdk && \
    tar -xzf target/package/shonin-strategy-sdk-*.crate -C /out/strategy-sdk --strip-components=1

# The builder compiles strategies, so its runtime keeps the pinned toolchain
# with the component target, and the SDK strategies are built against.
FROM rust:1.98.1-slim-bookworm
# Confidential Space's hardened image sends the container's lines to Cloud
# Logging only where the image allows it.
LABEL "tee.launch_policy.log_redirect"="always"
ARG NETWORK
# 1 where the image serves the network's EVM chains alone (no Solana ids in it).
ARG WITHOUT_SOLANA=
# The key holder (the executor's status port), the reviewer, and the API's
# internal port, by their internal names.
ARG EXECUTOR_URL
ARG REVIEWER_URL
ARG API_INTERNAL_URL
# Root inside the container: the launcher's token socket
# (/run/container_launcher/teeserver.sock) is the launcher's. Each build runs
# as nobody (65534), and the toolchain is left writable by nobody: a build
# can't change what the next one is compiled with.
RUN rustup target add wasm32-wasip2 && \
    chmod -R go-w /usr/local/rustup /usr/local/cargo && \
    mkdir -p /var/lib/shonin/store /var/lib/shonin/builds /var/lib/shonin/jobs && \
    chmod 0700 /var/lib/shonin/store /var/lib/shonin/builds && \
    chmod 0755 /var/lib/shonin /var/lib/shonin/jobs
# The SDK as a crate of its own (its workspace's fields written out), and the
# reviewed crates of config/dependency-allowlist.toml (fetched from crates.io
# and checked against their checksums): the only crates a build can reach.
COPY --from=build /out/strategy-sdk /opt/shonin/strategy-sdk
COPY --from=build /out/vendor /opt/shonin/vendor
RUN chmod -R go-w /opt/shonin
COPY --from=build /src/target/release/builder /usr/local/bin/builder
COPY config/chains.toml /etc/shonin/chains.toml
# The generation's deployment files: the named build context `deployments`
# (a build without it fails here).
COPY --from=deployments *.json /etc/shonin/deployments/${NETWORK}/
ENV SHONIN_NETWORK=${NETWORK} \
    SHONIN_WITHOUT_SOLANA=${WITHOUT_SOLANA} \
    SHONIN_CHAINS=/etc/shonin/chains.toml \
    SHONIN_DEPLOYMENTS_DIR=/etc/shonin/deployments/${NETWORK} \
    SHONIN_HOLDER_URL=${EXECUTOR_URL} \
    SHONIN_REVIEWER_URL=${REVIEWER_URL} \
    SHONIN_STATE_URL=${API_INTERNAL_URL} \
    SHONIN_BUILDER_JOBS=/var/lib/shonin/jobs
# Confidential Space opens only the ports an image declares.
EXPOSE 19084/tcp
ENTRYPOINT ["/usr/local/bin/builder"]
# Uploads come from the API, review requests go to the reviewer; the status
# port (/attestation) is read by the prover and the keeper. It exits once
# idle for ten minutes, and the VM powers off.
CMD ["--sdk", "/opt/shonin/strategy-sdk", "--vendor", "/opt/shonin/vendor", "--store", "/var/lib/shonin/store", "--builds", "/var/lib/shonin/builds", "--port", "19084", "--listen", "0.0.0.0", "--attestation", "launcher", "--idle-exit-seconds", "600"]
