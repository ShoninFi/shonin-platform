# Publishing

Publishing takes two steps in the web app: **submit** your agent, for $1, and once it's been reviewed and built, **activate** it.

## Sanity check

Once it's published, your agent can never change, so check it the way our builder will:

```bash
cargo test
cargo build --release --locked --target wasm32-wasip2
```

- **Commit your `Cargo.lock`.** The builder builds with `--locked`, so it uses exactly the dependency versions you tested with, and your archive only carries what you committed. The template already commits it; just don't add it to your `.gitignore`.
- **Keep the toolchain pinned** in `rust-toolchain.toml`, as the template does.
- **Build without the network, the clock or random numbers.** The builder is offline, and a build script that reaches for any of them counts against your agent's reproducibility rating.
- **Mind the limits.** Your source can be up to 2 MB unpacked, not counting locked dependencies or the AI models it ships, and the build can take up to 20 minutes.
- **Read your `shonin.toml` once more.** Its [description and claims](configuration.md#description) are what the Reviewer checks your code against, and what Depositors will read.

## Package your project

Commit your work, then pack it with git:

```bash
git archive --format=zip -o agent.zip HEAD
```

This works the same on every system, and packs exactly what you committed: nothing your `.gitignore` excludes, no `target` folder, and nothing you haven't committed yet.

## Submit it

In the web app, connect your wallet, choose `agent.zip`, and pay the $1. It's one transaction, on the chain your agent's [vault will live on](configuration.md#vault). The $1 covers the review and the build, and your agent is given its id. Only a payment from the wallet that uploaded the archive starts the job, so nobody else can claim your code.

Choose here whether to **seal** it. Your browser encrypts sealed source before it leaves your machine, so only our attested machines, and the AI model the Reviewer consults, can read it. [Keeping your code private](../authors.md#keeping-your-code-private) explains what that means for Depositors.

You never compute a hash. The attested machines hash what they receive themselves.

## Follow the job

Jobs are handled in order, one at a time for each Author. The job's page shows its place in the queue, then what it's doing, and finally how it ended. It never shows a build log, since that could give away sealed source.

It ends one of three ways:

- **Ready.** The review is posted and the build is registered. Nothing runs and nothing is listed yet: read the report, then [activate](#activate-it) your agent when you're happy with it.
- **Refused as malicious.** The report says why, and nothing is built.
- **The build fails.** To see why, run the same build yourself, as in the [sanity check](#sanity-check).

Whichever way it ends, the $1 is spent. After a refusal or a failed build, you try again by publishing a new agent.

## Activate it

When you're ready, activate your agent from its page, in one transaction on the same chain. There's no deadline. You choose:

- **Its allowances and idle floor:** the two [vault settings](configuration.md#vault-configuration). You can change both later.
- **A starting tank:** money for your agent's [tank](../tank.md). It has to be more than nothing, since an agent with an empty tank doesn't run, and about a week of running is a sensible start.

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
