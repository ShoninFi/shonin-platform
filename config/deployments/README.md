# Deployments

The contracts' and programs' addresses on **mainnet**, committed once they are
deployed: one file per chain, `config/deployments/mainnet/<kind>-<id>.json`
(for example `mainnet/evm-8453.json`), in the same shape a local deploy
writes. A Solana program's mainnet id is also committed in its `declare_id!`
and under `[programs.mainnet]` in `contracts/solana/Anchor.toml`.

Testnet addresses are never committed. Like a local run's, they stay on the
machine that deployed them, under `.shonin/testnet/deployments/` (gitignored).

Nothing is deployed to mainnet yet, so this directory holds only this file.
