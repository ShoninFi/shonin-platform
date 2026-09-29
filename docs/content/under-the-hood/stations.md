# Stations

A station is a vault's contract on a chain its agent trades on. It holds the money the agent trades with on that chain, owns the vault's accounts at the venues there, and reports what it all holds to the vault. It takes no deposits and issues no shares: Depositors only ever deal with the [vault](vaults.md).

Every vault has a station on each chain its agent trades on, its own chain included. [Smart contracts](../basics/smart-contracts.md#where-they-live) shows how they fit together.

## How one is made

**Beside the vault,** the station is created in the same transaction that activates the vault, so the two always exist together.

**On another chain,** the vault's [executor](machines.md#the-executor) creates it, through the station factory on that chain:

1. The executor's keys are [blessed](attestation.md#blessing-a-key) for this vault and its build, on that chain.
2. It asks the station factory for the vault's station. The factory only creates it for a key blessed as this vault's executor.
3. Once every station exists, the executor confirms it to the vault, and the vault opens.

Each station's address is fixed in advance from its vault, so asking twice just finds it already there. If an executor stops partway, the next one carries on from where it left off. A vault that's still not whole after three days can be cancelled by anyone: it never took a deposit, its tank goes back to the Author, and any stations already made hold nothing.

## Drivers

A station has a [driver](../writing/drivers.md) plugged in for each venue on its chain that its agent uses. The station itself knows nothing about any venue, but it owns the vault's accounts there and makes every call itself. Its drivers only prepare and check those calls: approving the vault's trade-only keys, working out what the station holds there, and closing positions when someone withdraws. [Drivers](../writing/drivers.md) explains them in full, including how each venue's money is kept in.

## One executor at a time

A station only ever approves one executor's keys at a time. When a standby takes over, approving its keys replaces the old ones, so two executors can never trade for the same vault at once.

## Stopping

When a vault is frozen, killed, or reaches the end of a retirement, every station hears about it, carried by the same attested machines and signed with a key blessed for the vault. Each station then withdraws its trading approvals and only lets positions be closed.

## What it's worth

A station works out what it holds from its own chain, the way that chain's venues report it: positions and balances straight from the venue's on-chain state, and prices from the venue's own oracle or a public price feed. Nobody reports a number to it.

Getting that figure to the vault depends on where the station is:

| Where the station is | How the vault learns what it holds |
| --- | --- |
| **On the vault's own chain** | The vault reads it directly, in the same transaction |
| **On another chain** | The vault's executor, or any unwinder blessed for the vault, reads it there, at that chain's first block at or after the vault's [valuation time](vaults.md#priced-after-you-ask), and carries it to the vault, signed. The vault only accepts it from a key blessed for this vault, and only for that valuation time |
| **At a venue that keeps its accounts off-chain** | An unwinder reads the account through the venue's own service and signs its value. The vault's page marks this as an **attested read**, one step below a value worked out on-chain |

A figure is carried when someone needs it, for a deposit or withdrawal waiting on a valuation, so there's no standing cost. If two carried figures for the same moment disagree, deposits use the higher and withdrawals the lower. An unwinder can carry it too, and any approved operator can run one, so in time a withdrawal won't depend on us; at launch, we're the only operator. Each vault's page shows how much of its value is on its own chain, carried from another chain, or an attested read.

## Moving money

Money only ever moves between the vault and its stations, and between a station's own accounts at its venues. Every destination is one of the vault's own, fixed when the vault is created, and the contracts refuse anything else.

- **Between chains,** money only travels over the [bridges](../writing/bridges.md) the agent names, and the station itself checks, on the chain, that it will arrive at the vault's own address on the other side.
- **Money in transit** still counts in the vault's value for up to an hour. After that it counts as zero until it arrives, and the vault's page flags it.
- **The idle floor** limits what can leave the vault at all: the vault won't send money to its stations if that would leave less than the Author's floor at home.

## Getting money out

When a withdrawal needs positions closed, it depends on where they are:

| | When | Who closes the positions |
| --- | --- | --- |
| **Direct** | Every station is on the vault's own chain | The contracts alone. Anyone can send the transaction that settles the claim |
| **Served** | Any station on another chain, or any venue that keeps its accounts off-chain | An [unwinder](machines.md#the-unwinder), run by any approved operator, with keys blessed for this vault. It closes the positions, brings the money home over the declared bridges, and is repaid its costs from that money |

A served vault's page shows how many operators can run an unwinder for it, and what each venue's last resort is if none does.

## What the chain verifies

| Fact | Checked by the chain | Rests on |
| --- | --- | --- |
| The station belongs to this vault, at its fixed address | ✓ | |
| Money only moves to the vault's own accounts | ✓, by the station and the venue | |
| Which keys may trade, and that they can't withdraw | ✓ | Each key's blessing, and the venue's own rules |
| What the station holds | ✓, worked out on its own chain | The prices it uses; for an off-chain venue, the unwinder's attested read |
| That figure, at the vault | On the vault's chain, directly; from another chain, signed and fresh | The attested machine that carried it |

[Next: registries →](registries.md){ .md-button .md-button--primary }
