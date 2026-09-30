# syntax=docker/dockerfile:1
# The builder: compiles submitted source into a signed bundle.
# Runs on a Confidential Space VM, which attests to this image by its digest.
# Built from the repository root: task cloud:build NAME=builder
#   (docker build -f deploy/gcp/Dockerfile.builder .)

FROM rust:1.98.1-slim-bookworm AS build
WORKDIR /src
COPY . .
# rust-toolchain.toml pins the same compiler; --locked builds exactly Cargo.lock.
RUN cargo build --release --locked -p shonin-builder --bin builder

# The builder compiles strategies, so its runtime keeps the pinned toolchain
# with the component target, and the SDK strategies are built against.
FROM rust:1.98.1-slim-bookworm
RUN rustup target add wasm32-wasip2
COPY packages/strategy-sdk /opt/shonin/strategy-sdk
COPY --from=build /src/target/release/builder /usr/local/bin/builder
COPY config/chains.toml /etc/shonin/chains.toml
# Only mainnet addresses are committed, so only they are baked in; the network
# (SHONIN_NETWORK) and its deployments directory (SHONIN_DEPLOYMENTS_DIR) are
# set where the image is launched.
COPY config/deployments/mainnet /etc/shonin/deployments/mainnet
ENV SHONIN_CHAINS=/etc/shonin/chains.toml
USER nobody
ENTRYPOINT ["/usr/local/bin/builder", "--sdk", "/opt/shonin/strategy-sdk"]
