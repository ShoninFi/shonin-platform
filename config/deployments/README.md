# Deployments

The contracts' addresses on public chains, committed once they are deployed:
one file per chain, `config/deployments/<network>/<kind>-<id>.json` (for
example `testnet/evm-84532.json`), in the same shape a local deploy writes.

Nothing is deployed to a public chain yet, so the network directories are
empty. A local run writes its own files under `.shonin/local/deployments/`,
which is gitignored.
