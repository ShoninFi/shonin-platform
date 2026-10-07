# syntax=docker/dockerfile:1
# The fuel benchmark, for timing a machine agents run on. Built from the
# repository root:
#   docker build -f packages/fuelbench/Dockerfile -t fuelbench .
#   docker run --rm fuelbench                      # one processor at a time
#   docker run --rm fuelbench --threads 4          # all four of a 4-vCPU machine busy
# On the executor's machine type, the second is the figure to price from:
# each processor is timed with its neighbours (a hyperthread's sibling
# among them) as busy as they are on a full machine.

FROM rust:1.98.1-slim-bookworm AS build
WORKDIR /src
COPY . .
# rust-toolchain.toml pins the compiler and the wasm32-wasip2 target.
RUN cargo build --release --locked -p shonin-fuelbench-guest --target wasm32-wasip2 && \
    cargo build --release --locked -p shonin-fuelbench && \
    mkdir -p /out && \
    cp target/release/fuelbench target/wasm32-wasip2/release/shonin_fuelbench_guest.wasm /out/

FROM debian:bookworm-slim
COPY --from=build /out/ /opt/fuelbench/
ENTRYPOINT ["/opt/fuelbench/fuelbench", "rate", "--guest", "/opt/fuelbench/shonin_fuelbench_guest.wasm"]
