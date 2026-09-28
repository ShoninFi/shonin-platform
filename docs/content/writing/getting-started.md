# Getting started

An agent is an ordinary Rust project that compiles to one **WebAssembly component**. It runs as a single long-lived program: it watches the market, decides what to do, and hands the attested machine its orders to sign and send. It never holds a key, and it can only ever trade its vault's own accounts.

The quickest way to write one is to start from our template: a complete, small agent you can build, test and publish as it is, then change into your own.

## Set up

1. **Get a copy.** On GitHub, open [`ShoninFi/shonin-agent-template`](https://github.com/ShoninFi/shonin-agent-template) and choose **Use this template**, or clone it:

    ```bash
    git clone https://github.com/ShoninFi/shonin-agent-template my-agent
    cd my-agent
    ```

2. **Install Rust** from [rustup.rs](https://rustup.rs). The template pins its own toolchain and the `wasm32-wasip2` target, so `cargo` fetches both the first time you build.

3. **Test and build it:**

    ```bash
    cargo test                                        # the agent's decisions, run natively
    cargo build --release --target wasm32-wasip2      # the component, the way our builder makes it
    ```

4. **Follow the README.** It walks through the example file by file, and says what to change first to make it yours.

5. **Publish it** when you're ready: [Publishing](publishing.md) has the steps.

## What's inside

| File | What it is |
| --- | --- |
| `shonin.toml` | The agent's [configuration](configuration.md): its description, its claims, where it trades and the bridges it uses |
| `src/config.rs` | Its settings, as constants: the market, the target, and the numbers behind its limits |
| `src/lib.rs` | The agent itself: its one long-lived `run` |
| `src/decide.rs` | Its decisions, as plain functions of what it sees, so they can be tested without a venue |
| `tests/` | Tests for those decisions |
| `Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml` | The dependencies and toolchain, pinned, so our builder compiles exactly what you did |

Keeping the decisions apart from the waiting and the ordering is the one habit worth copying. It means you can test everything that matters with plain `cargo test`, on your own machine, before any money is involved.

## The SDK

The template depends on `shonin-strategy-sdk`, which has everything an agent needs to talk to its venues and its vault, plus helpers for common jobs.

[Next: configuration →](configuration.md){ .md-button .md-button--primary }
