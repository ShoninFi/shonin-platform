# Publishing

Publishing takes two steps in the web app: **submit** your agent, for 1 USDC, and once it's been reviewed and built, **activate** it.

## Sanity check

Once it's published, your agent can never change, so check it the way our builder will:

```bash
cargo test
cargo build --release --locked --target wasm32-wasip2
```

- **Commit your `Cargo.lock`.** The builder builds with `--locked`, so it uses exactly the dependency versions you tested with, and your archive only carries what you committed. The template already commits it; just don't add it to your `.gitignore`.
- **Depend only on reviewed crates.** Besides the SDK and crates inside your own project, your agent may depend only on the crates.io crates the platform has reviewed itself, each at an exact version, listed in [`config/dependency-allowlist.toml`](https://github.com/ShoninFi/shonin-platform/blob/main/config/dependency-allowlist.toml). The template's `Cargo.lock` uses only those. A git dependency, another registry, an unlisted crate or version, or a path outside your project is refused when you upload, before you pay. To use another crate, ask for it to be reviewed and added.
- **Keep the toolchain pinned** in `rust-toolchain.toml`, as the template does.
- **Build without the network, the clock or random numbers.** The builder is offline, and a build script that reaches for any of them counts against your agent's reproducibility rating.
- **Mind the limits.** Every file in your project counts: up to 2 MB unpacked, and up to 768 KiB as the Reviewer reads it (each file named, each line numbered), since it reads your project whole or not at all. Real agents are a small fraction of that. The build can take up to 20 minutes.
- **Read your `shonin.toml` once more.** Its [description and claims](configuration.md#description) are what the Reviewer checks your code against, and what Depositors will read.

## Package your project

Commit your work, then pack it with git:

```bash
git archive --format=zip -o agent.zip HEAD
```

This works the same on every system, and packs exactly what you committed: nothing your `.gitignore` excludes, no `target` folder, and nothing you haven't committed yet.

## Submit it

In the web app, connect your wallet, choose `agent.zip`, and pay the 1 USDC. It's one transaction, on the chain your agent's [vault will live on](configuration.md#vault). The 1 USDC covers the review and the build, and your agent is given its id. Only a payment from the wallet that uploaded the archive starts the job, so nobody else can claim your code.

Choose here whether to **seal** it. Your browser encrypts sealed source before it leaves your machine, so only our attested machines, and the AI model the Reviewer consults, can read it. [Keeping your code private](../authors.md#keeping-your-code-private) explains what that means for Depositors.

You never compute a hash. The attested machines hash what they receive themselves.

## Follow the job

Jobs are handled in order, one at a time for each Author. The job's page shows its place in the queue, then what it's doing, and finally how it ended. It never shows a build log, since that could give away sealed source.

It ends one of three ways:

- **Ready.** The review is posted and the build is registered. Nothing runs and nothing is listed yet: read the report, then [activate](#activate-it) your agent when you're happy with it.
- **Refused as malicious.** The report says why, and nothing is built.
- **The build fails.** To see why, run the same build yourself, as in the [sanity check](#sanity-check).

Whichever way it ends, the 1 USDC is spent. After a refusal or a failed build, you try again by publishing a new agent.

## Activate it

When you're ready, activate your agent from its page, in one transaction on the same chain. There's no deadline. You choose:

- **Its allowances and idle floor:** the two [vault settings](configuration.md#vault-configuration). You can change both later.
- **A starting tank:** money for your agent's [tank](../tank.md). It has to be at least $10, plus the cost of creating its stations on any other chains, which the web app shows, and about a week of running is a sensible start.

Before you activate, seal any [secrets](services.md#secrets) your agent needs, such as an API key for a paid data service.

If your agent trades on more than one chain, its vault opens once its [stations](../basics/smart-contracts.md#where-they-live) on the other chains are ready, usually within minutes. The web app shows how far along it is.

## Once it's live

Your vault takes deposits straight away, marked **new** until it has some history. Its page shows everything a Depositor needs to check it:

- the review, with its ratings and findings;
- its vault's chain, and your description, claims, drivers and bridges;
- the source, unless it's sealed, and its toolchain;
- the hashes that tie the source to the code running;
- which attested machines are allowed to run it;
- its record, its fees and its tank.

From then on, keep the tank topped up, or switch on [auto-fill](../tank.md#filling-it) and let your earnings do it.

[Next: retiring →](retiring.md){ .md-button .md-button--primary }
