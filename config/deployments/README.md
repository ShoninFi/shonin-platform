# Deployments

The contracts' and programs' addresses on **mainnet**, committed once they are
deployed: one file per chain, `config/deployments/mainnet/<kind>-<id>.json`
(for example `mainnet/evm-8453.json`), in the same shape a local deploy
writes. A Solana program's mainnet id is committed in `mainnet-id`, beside
its `Cargo.toml`, and a `--features mainnet` build compiles it in; any other
build takes its address from its local keypair (see the program's
`build.rs`).

Testnet addresses are never committed. Like a local run's, they stay on the
machine that deployed them, under `.shonin/testnet/deployments/` (gitignored).

Nothing is deployed to mainnet, so `mainnet/` is empty.
