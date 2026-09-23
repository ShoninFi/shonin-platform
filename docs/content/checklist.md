# Build checklist

Everything between here and a platform running on a public chain, grouped by area, with no dates. This is the working guideline: tick a box when it is done, add items as work appears, and keep it honest.

!!! info "What a tick means"
    **`local`** — built and working on the local development chain, against its stand-in venue and development attestation verifier. It is not a tick: the mechanism is proved, the environment is not.

    **`[x]`** — done for real, on the chain and the venue it names.

    A card with neither is not started.

---

## Guarantees first

The guarantees are the product. Everything else is how they are shown.

- [ ] `local` **Funds can only move between a vault's own accounts**, enforced by the venue and by the vault contract, with no route out for any key
- [ ] `local` **Anyone can exit without us**: `redeem` pays from idle balance, or books a claim that `settle` — a public function — completes
- [ ] `local` **Only the committed bundle can use a vault's keys**, checked against the registry on every approval
- [ ] `local` **The record is computed by the vault contract** from on-chain state, never reported
- [ ] `local` **Every image that may hold a key is on a public, timelocked allowlist**, reproducible from published source
- [ ] Each guarantee demonstrated publicly, end to end, on a real chain

## Contracts

- [ ] `local` **Vault and factory**: ERC-4626 shares, deposit at mark, redeem at exit-side marks, claims with pending shares, capital moved only between registered accounts
- [ ] `local` **Fees and the tank**: daily crystallisation as shares, the high-water mark, the transaction fee, tank funding, auto-fill, usage charges
- [ ] `local` **Claims and kills**: settle inside the exit band, freeze after an unserved claim, an admin kill that returns everything to depositors
- [ ] `local` **Attestation registry**: the timelocked allowlist by image kind, key blessing from a proof, a Merkle proof per vault
- [ ] `local` **Strategy registry**: registration only with a valid builder signature, supersession, retirement
- [ ] `local` **Review registry**: signed reviews from blessed reviewers, and anyone buying a fresh one
- [ ] `local` **Permissionless venue approvals** that obey only the registry
- [ ] **Gas stocking**: funding an executor key's gas on chains that charge it, and repaying whoever funded it
- [ ] **Audits** of every contract, with the status of each shown on every vault page from day one

## Execution

- [ ] `local` **The runtime**: one long-lived instance per vault, fuel and epoch yields, a memory cap, restart after a fault, an ephemeral per-strategy cache
- [ ] `local` **Signed health reports**: restarts, faults, refused submissions, venue errors and tank runway, refused from any key not blessed for that vault
- [ ] `local` **Signed daily usage statements**, capped by the allowances the author declared at the published prices
- [ ] `local` **Blessing and renewal**, with the venue's approval following the blessing
- [ ] **A real enclave**: keys generated inside it, a genuine attestation token, and the on-chain proof of that token
- [ ] **The root key**: generated in the enclave, released only by attested key exchange to an allowlisted image, with derived per-bundle and per-vault keys
- [ ] **Image rotation by failover**: a standby on the new image blessed and taking over, the previous known-good image kept allowlisted, propose before revoke
- [ ] **Network for strategies**: host-terminated TLS and websockets under per-strategy quotas, with the declared bandwidth allowances actually metered
- [ ] **Model inference**: the runtime, a time budget and a watchdog, and models loaded from the bundle
- [ ] **Many vaults per machine**: placement by allowance, standbys, and failover that restarts a strategy which rebuilds from venue state
- [ ] **A published unwinder image** with the venue adapters' unwind paths only, runnable by another operator
- [ ] **Audits** of every image

## Build and review

- [ ] `local` **The queue**: first come first served, one job per author, the full log whether it succeeded or failed, and a position that is counted when it is asked for
- [ ] `local` **Admission and charges**: a job admitted once its vault exists and its tank covers the minimum; a build charged because it ran; a failed build closing the vault and returning the rest
- [ ] `local` **The signature** binding source, manifest and bundle to the chain, the registry, the strategy and its author
- [ ] `local` **The review**: a pinned model, prompt, schema, categories and severities, signed and recorded, with anyone able to buy a fresh one
- [ ] **A calibrated score**: a second model answers a fixed battery of narrow questions and the score is computed from the answers with published weights, so a reader can see which question moved it. The battery, the client and the comparison harness are built; measuring it needs an account
- [ ] **A calibration set**: a fixed set of strategies with agreed scores, re-run before any reviewer image is proposed, so a change to the model or the prompt has to show what it did to them
- [ ] **A hermetic build**: a published, pinned builder image with an offline dependency snapshot, remapped paths and a fixed timestamp, so an outsider's rebuild matches ours byte for byte. Until this is done, a rebuild can differ for reasons that have nothing to do with the source
- [ ] **Reproducibility shown rather than asserted**: rebuild published bundles off the critical path, deliberately varying the machine, the path and the date, and show the result on the strategy page
- [ ] **Rebuild attestations from outsiders**, so corroboration accrues publicly instead of resting on our word
- [ ] **Submission from a public repository and commit**, cloned and hashed by the build service
- [ ] **Models in a bundle**: shipped, hashed, loaded at build time and reviewed alongside the code
- [ ] **A version diff** for a strategy that declares what it supersedes
- [ ] **A pre-review** an author can run before publishing, through the same prompt and schema
- [ ] **Build reproducibility in practice**: once the recipe is hermetic, do real projects, with their proc macros and build scripts, rebuild to the same bytes on a different machine?

## Venues and custody

- [ ] `local` **A venue adapter**, with the shared payload types and decimal arithmetic every strategy uses
- [ ] **A perpetuals venue where a vault contract can be the master of its own account**, add a trade-only agent, read account value on-chain, and close reduce-only for a claim
- [ ] **An equities venue**, with holdings valued from a price feed under a stale-feed rule
- [ ] **A second chain**, with a remote-leg contract that owns the vault's accounts there, values them from chain state, and posts home on demand
- [ ] **A transfer route between chains**, with per-route caps and timeouts
- [ ] **Attested reads** for venues whose state is not on a chain
- [ ] For each venue: its custody level, shown on every vault page that uses it

## The record and the API

- [ ] `local` **The indexer**: returns, drawdowns and volatility from each vault's value per share, open source so anyone can run their own
- [ ] `local` **The public HTTP API** behind every page
- [ ] **Oracle monitors** that flag a stale or anomalous price behind a vault's valuation in its health report
- [ ] **A documented, versioned API** with stability guarantees

## The web app

- [ ] `local` **Leaderboard and strategy pages**: on-chain records, review scores, normalised returns, the size each record was earned at, the author's whole history
- [ ] `local` **Source, build log and toolchain** for every strategy
- [ ] `local` **The build queue**, with a public log for every job
- [ ] `local` **Deposits, withdrawals and claims**, with a pending claim's state shown
- [ ] `local` **Health reports** on every vault page, with warnings and errors surfaced
- [ ] `local` **"Which code is trading this vault?"**, including the audit status of every contract and image
- [ ] `local` **Publishing from the browser**: pack, create the vault, submit, follow the queue
- [ ] **Sealing a secret in the browser** to the key the registry publishes
- [ ] **A real wallet flow** against a public chain, with network switching and clear failure states
- [ ] **Accessibility and mobile** passes on every page

## Authoring

- [ ] `local` **The SDK**: the frozen world, typed helpers for each interface, decimals, and a venue's payload types
- [ ] `local` **A working reference strategy**, with its limits in its own code
- [ ] **Template repositories** for a few recognisable strategy shapes
- [ ] **Helper crates** for the plumbing a strategy should not rewrite: hedging two legs, working an order over time, quoting from fair value, rebalancing margin
- [ ] **Generated API documentation** for the SDK, published
- [ ] **An authoring guide** that takes someone from `cargo new` to an open vault

## Numbers to measure

Working defaults are in the [parameters](reference/parameters.md); these need measuring before they can be trusted.

- [ ] **Fuel to processor time** on real machines, and what the yield interval costs
- [ ] **Model limits**: load time and per-call latency for representative models
- [ ] **Bandwidth**: what real strategies actually consume against their declared ranges
- [ ] **Unit prices**: what compute actually costs, and how many strategies fit on one machine, to set the published prices
- [ ] **Executor latency** to each venue, to set placement defaults

## Operations

- [ ] `local` **The keeper**: daily crystallisation, settling claims, freezing when due
- [ ] `local` **A public status feed**: every kill, every allowlist change, every venue that stopped behaving
- [ ] **Monitoring and alerting** for the services, separate from the on-chain guarantees
- [ ] **A runbook** for a revoked image, a failing venue and a stuck claim
- [ ] **Key management** for the admin key, and a path to a multisig

## The site

- [ ] **Publish the repository.** Most of what this site claims — rebuild it yourself, run your own indexer, read the review prompt, run the whole platform locally — is worth nothing while the source is private. Near the top of the list for that reason
- [ ] `local` **Pre-rendered pages, a sitemap and robots.txt**, so every route has real content without JavaScript
- [ ] **A link preview image**

    ??? note "What that needs"
        - A 1200×630 PNG, with text and logo inside the central safe area, since some platforms crop to a square.
        - In the pre-render, emit `og:image` (absolute URL), `og:image:width`, `og:image:height` and `og:image:alt`, plus `twitter:image`.
        - Switch `twitter:card` from `summary` to `summary_large_image`.
        - Consider per-route images later, such as a generated card per strategy page.

- [ ] **Social profiles**, once they exist: `twitter:site`, and the profile URLs as `sameAs` on the home page's structured data
- [x] **These docs**, deployed
- [ ] **A public verification guide** that someone outside can follow end to end

## Launch

- [ ] **Reference vaults on a public chain**, open for deposits
- [ ] **Demonstration: exit without us.** Stop every executor, then a depositor withdraws more than the idle balance, sends the settle transaction themselves, and is paid
- [ ] **Demonstration: the blocked exit.** A strategy tries to send most of a vault to an outside address and fails, because no such route exists
- [ ] **Demonstration: a record nobody can backfill**, with every trade on-chain and returns read straight from the vault contract
