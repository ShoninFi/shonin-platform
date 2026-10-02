# Vaults

A vault is the one contract behind each agent that Depositors deal with. It lives on the chain its Author chose, takes deposits, pays withdrawals, issues shares, charges fees and holds the [tank](../tank.md). It never trades: its [stations](stations.md) do. It works out its own share price, from its own cash and what its stations hold. For a station on another chain, that figure is carried to it by our attested machines.

## Its life

```mermaid
stateDiagram-v2
    [*] --> Activating: the Author activates
    Activating --> Live: every station exists
    Activating --> Cancelled: not whole after 3 days
    Live --> WindingDown: the Author retires it
    WindingDown --> Closed: 7 days later, positions closed
    Live --> Frozen: a claim unpaid for 7 days
    Live --> Killed: the admin key kills it
    Frozen --> Killed
    Frozen --> Closed: positions closed
    Killed --> Closed: positions closed
```

| State | Deposits | Withdrawals | The agent |
| --- | --- | --- | --- |
| **Activating** | No | Nothing to withdraw | Not started, while its stations are created |
| **Live** | Yes | Yes | Running |
| **Winding down** | No | Yes | Running, for 7 days |
| **Frozen** | No | Yes | Only unwinding is left |
| **Killed** | No | Yes, once the positions are closed | Stopped |
| **Closed** | No | Yes, from cash, whenever anyone likes, with no fees | Stopped for good |
| **Cancelled** | Never took any | Nothing to withdraw; the tank goes back to the Author | Never started |

A vault never changes its agent. A changed agent is a new agent, with a vault of its own.

## Shares

A deposit mints shares at the vault's value per share, and a withdrawal burns them. Your profit or loss is your shares times today's value per share, less what you paid.

On Ethereum-style chains, a vault's shares use **ERC-4626**'s maths, and deposits and withdrawals are requests (below). On Solana, the vault is a Solana program with the same rules. On every chain, **shares can't be transferred**: you get money out by withdrawing.

Whoever is moving pays for the move, never the pool:

| Moving | Valued at |
| --- | --- |
| Depositing | Mark, or the exit side if that's higher, so a deposit never buys below what the holdings could be sold for |
| Withdrawing | Exit-side marks: a long at the best bid, a short at the best ask, never above the mark |

**An empty vault can't be gamed.** A classic trick against a new vault is to deposit a tiny amount, then send the vault a large sum directly, so the next depositor's shares round down to nothing. It doesn't work here: the share maths always counts a small number of phantom shares, a vault only counts money that came in through its own deposits and its stations, and a sliver of shares that nobody can withdraw is created when the vault is activated.

## Priced after you ask

**Every deposit and withdrawal is a request, priced a few seconds after you make it.** When you ask, the vault records your request and a **valuation time**: the first ten-second mark at least five seconds away. You're then valued at the prices at that moment, never at a price that was already known when you asked. Nobody, you included, can see that price when committing to it, so nobody can race the vault's own figure with news from elsewhere.

- **A request can't be cancelled.** Otherwise it would be a free bet: ask, then cancel if the price moved the wrong way. The one exception is a deposit nothing has priced for a day, which you can take back.
- **Which prices count is fixed by a rule, not by whoever settles.** Prices on the vault's own chain must be recorded within three seconds after the valuation time. A station on another chain is valued at that chain's first block a slot or more after the valuation opened, and an attested machine carries that figure to the vault within the next minute, signed with a key blessed for this vault. If nothing is recorded in time, the request moves to the next valuation time.
- **One slot, one price.** Everything asked for the same valuation time is priced at that valuation. Its withdrawals are settled first, whatever order they were asked in, at the valuation's exit-side price and only from the cash it saw. Its deposits come after, all at one price: the pool as those withdrawals left it. So a leaver meets the pool exactly as the valuation found it, and a newcomer neither pays for nor shares in the gap between what the leavers were paid and the mark.
- **A valuation at a price nobody can trust prices nothing.** If any market the vault holds is further from the venue's own price than the [exit band](#the-exit-band), the valuation is **void**: no deposit, withdrawal or fee is priced at it, and its requests roll on to the next one. The same goes for a station on another chain that never reported, or money on a bridge past its timeout.
- **If two carried figures for the same moment disagree,** deposits use the higher, withdrawals the lower, and fees the lower mark, so a wrong figure can't help anyone take money from the pool.

Anyone may record a valuation and settle what it priced (`recordValuation`, then `settle`); our keeper does it every slot so nobody has to. The web app does the asking for you, and a deposit or withdrawal paid from cash is still done within seconds. The smallest deposit is $10.

**A deposit carries a signed statement.** The depositor's wallet signs that they aren't one of the people this vault excludes and aren't acting for one, and the deposit request carries that signature. The vault works out its list from its drivers, plus US persons and sanctioned persons, plus anything its Author added; it checks the signature against that list, records it, and refuses a deposit without one, however the deposit arrives. If the list grows before a deposit is priced, and the depositor hasn't signed for the longer one, the deposit is sent back. A withdrawal never needs one. [Depositors](../depositors.md#who-can-deposit) and [Drivers](../writing/drivers.md#who-a-venue-excludes) have the rest.

!!! note "Not quite ERC-4626"
    Deposits and withdrawals settle after a request, in the manner of ERC-7540. Anything integrating with a vault should expect that.

## Its value

```text
vault value = cash in the vault
            + what each station holds
            − the tank (the Author's money for running the agent, not Depositors')
```

The vault reads a station on its own chain directly. A station on another chain works out what it holds from its own chain, and an attested machine carries the figure to the vault. [Stations](stations.md#what-its-worth) explains how each is valued. Money crossing a bridge counts at face value for up to an hour. Past that, a valuation prices nothing until it lands, so nobody can buy cheap while it counts for nothing and sell once it arrives.

## Leaving

```mermaid
flowchart TB
    R(["ask to withdraw"]) --> T["wait for the<br/>valuation time"]
    T --> C["pay the profit fee<br/>these shares owe"]
    C --> V["value at exit-side marks,<br/>take the transaction fee"]
    V --> Q{"enough cash<br/>in the vault?"}
    Q ==>|yes| P(["burn the shares, pay"])
    Q -->|no| B["keep the shares pending,<br/>book a claim"]
    B --> S["<b>settleClaim</b>: anyone may send it"]
    S -->|"the cash covers it now"| T
    S -->|"otherwise"| H["close your share of every position<br/>inside the exit band, and pay you<br/>in the same transaction"]
    H --> P2(["paid"])

    classDef fast fill:#0d948818,stroke:#0d9488,color:inherit
    classDef slow fill:#f59e0b18,stroke:#f59e0b,color:inherit
    classDef ends fill:#4338ca18,stroke:#4338ca,color:inherit,font-weight:bold
    class T,C,V fast
    class B,S,H slow
    class R,P,P2 ends
```

**Most withdrawals are paid from cash.** Each vault keeps some of its money at home as cash, its **idle floor**: 5% by default, set by the Author, and enforced by the vault, which won't let money out to its stations if that would leave less. Lowering the floor takes a day, so Depositors see it coming.

**When there isn't enough cash,** the shares stay pending and the vault books a **claim**. Settling it closes the leaver's share of every position, inside the exit band, and pays them in that same transaction: what the close brought home, plus their share of the cash as it stood just before, less the profit fee their own shares owe (paid in cash to the Author and Shonin) and the transaction fee (kept in the pool). Their shares are burned there and then. No price anyone could pick goes into it: the amount comes from a real sale and the real cash.

If the cash covers the claim by the time it's tried, nothing is closed: the claim is **queued** and priced at the next valuation, like any withdrawal. A claim is *pending*, *waiting* if the market was outside the exit band when it was tried, *queued*, *paid*, or taken through the fallback below.

**Settling needs nobody's permission.** `settleClaim` is public: if everything we run stopped this minute, a Depositor could send it themselves. On the vault's own chain the contracts finish it alone. Where a station is on another chain, the claim is paid once an [unwinder](machines.md#the-unwinder) has brought its share home and the cash covers it. Any approved operator can run an unwinder, though at launch that's only us. Each vault's page says which kind of vault it is, **direct** or **served**, and [Stations](stations.md#getting-money-out) explains the difference.

**If nothing can be priced for a day,** say a market sits outside the exit band, or a station's figure can't be carried because no operator is running, a withdrawal isn't stuck. Once a withdrawal has gone a day without a valuation pricing it, you choose:

- **The fallback.** Take your share of the vault's cash straight away, and **units** for the same share of what the stations hold, paid as that money comes home or as your units are served by closing your share of each position.
- **A claim instead** (`claimInstead`), booked from when the withdrawal was due, so it counts toward a [freeze](#a-freeze) from then. A close is tried for it like any other claim.

A claim nobody could serve for a day after it was booked can take the fallback too, unless the cash already covers it. Either way the fallback isn't a way round a fee: it pays the profit fee your shares owed at the last priced valuation, as a slice of them, and the transaction fee on the cash it pays, kept in the pool. Nothing else needs pricing, so nothing can be gamed, and nobody else in the pool pays for it.

### The exit band

A claim's positions are closed with reduce-only orders that fill at once or not at all, at the venue's oracle price plus or minus the **exit band**, 1% by default. Every venue's close is checked before any is made, so a market outside the band on one venue closes nothing anywhere: a leaver is never half-sold. If the market is outside it, or the venue refuses the close, nothing fills, the claim waits, and settling is tried again.

The same band guards the cash. A valuation at which any market sits outside the band is void, so nobody who widens a spread for a moment can be paid at it, or make anyone else be.

**After a vault stops,** its stations are unwound in full, shared by everyone, each venue on its own. For the first week the same band applies, so nobody's money is dumped into a market that is only briefly broken; after that the band doubles every week, so a market that stays broken for good (a delisted token, a stale price) can't keep the money at the venue forever. A close still takes the best price on offer; the wider band only matters when the market really is that thin. If one venue can't be closed at all, the others are, and every idle dollar still comes home.

The leaver pays the spread, the transaction fee, and the costs of their own exit. A bad fill falls on the leaver alone: they're paid what the close actually brought home, never more than their share was worth at exit prices before it, and if a venue keeps the proceeds instead of sending them home, the whole close is undone. Where a share has to be brought home from another chain, the unwinder's gas and bridge fees come out of that leaver's payout too. The unwinder reports them, signed with its blessed key, and they can never be more than a fixed cap per claim, set in the vault's terms.

### A freeze

**A claim still unpaid seven days after it was booked lets anyone freeze the vault,** whatever was tried meanwhile: no new deposits, the agent's trading keys lose their approval at every station, and only unwinding is left. So no market, and no strategy, can keep a leaver out for good.

## Fees

Fees are measured at every valuation and paid on the first valuation on or after Monday 00:00 UTC, as **new shares**, so paying them never forces a sale. A void valuation is never a payday.

```text
at every valuation, what is owed so far is worked out and taken out of value per share:

  holding fee  = value × 1% × (time since the last valuation) / 365 days
  if value (after the holding fee) > high-water mark × shares:
      gains      = value − high-water mark × shares
      author fee = gains × 10%
      platform   = gains × 5%

at the first valuation on or after Monday 00:00 UTC:
  what is owed then is paid as new shares          → the Author, the platform
  high-water mark = value per share, if that is a new high

on every deposit:
  transaction fee = 0.1% of the amount              → stays in the pool
  money joining above the high-water mark joins at its own price:
  the mark becomes the average, so nobody else's fee or price moves

on every withdrawal:
  transaction fee = 0.1% of the amount              → stays in the pool
  the leaver pays the profit fee their own shares owe, with a slice of those shares
  (a claim paid at its close pays that fee in cash) → the Author, the platform
```

- **Value per share is always after fees.** What's owed so far is already taken out of the number a Depositor sees, so there's no drop on payday.
- **Fees owed can shrink.** If the vault gives back its gains before the week is out, what it owes shrinks with them. A gain that vanishes within the week is never charged for.
- **The high-water mark is the vault's own:** its highest value per share at a weekly payment, averaged up when money joins above it. Profit fees are only ever taken on new highs, and money that joins never pays for gains it wasn't there for.
- **A leaver pays their own fee.** Leaving settles only the profit fee the leaver's shares owe, so nobody else's fee comes due early, and nobody else's price moves.
- **The transaction fee is nobody's.** It stays in the pool and raises value per share for the Depositors who stay, so timing a deposit or withdrawal around a price update costs the one timing it.

The schedule is fixed when the vault is created and never changes under it. A vault's page shows its own schedule, not today's default.

## The tank

Each vault has a **tank**: a plain dollar balance, held by the vault but not counted in what shares are worth, that pays for running the agent. The Author fills it when activating, and whenever they like after that.

- **What's charged is capped.** The executor keeps a running total of what the agent uses, signed with the vault's own blessed key, and stops the agent the moment the unpaid part reaches the tank. Once a day, `keep` collects it and tops the tank up if the Author asked for that. Anyone may send it; Shonin normally does, and pays its gas. The vault only believes the total up to the Author's allowances at our published prices, for the time that has passed, so a wrong total can never charge more than the Author agreed to.
- **Earnings can pay for it.** The Author's fee shares are theirs. They can turn some into tank money, or set **auto-fill**, "keep my tank at $X", and each day just enough of their shares are turned into cash to top it up. Either way the shares are redeemed like any withdrawal, at the next valuation and only from cash, never as a claim, so nobody else pays for it.
- **The Author can take it out** at any time. The latest bill, signed by the executor, is paid first, and the rest comes back at once. If no executor is running to sign a bill, the Author asks, waits a day so anyone can collect a final bill, and then has a day to take it out without one; after that they ask again. Once the vault has closed, there's no wait.
- **A dry tank stops the agent,** until anyone tops it up. Its positions stay as they are, and the vault's page says so. Withdrawals never draw on the tank, so an empty one can't hold anyone up. A final bill bigger than the tank takes what's there, and the rest is written off.

## Retiring

An Author can **retire** their agent at any time, but it takes a week on purpose. The vault stops taking deposits at once, the agent keeps running, and withdrawals work as usual. After seven days anyone can end it: the agent is sent `halting` and gets ten minutes to wind down, then it's stopped for good, every position is closed the same way a claim closes them, and the cash comes home, its costs paid from the money itself. The vault stays on the chain for good, holding only cash, for anyone still in to withdraw.

## A kill

Shonin's **admin key** can **kill** a vault, typically one whose health report shows it failing. It's the only thing that key can do to a vault. The agent is sent `halting` and gets ten minutes to wind down itself, then every position is closed the same way a claim closes them, and the cash comes home, so every Depositor can then be paid in one transaction.

A kill moves no money anywhere but back to Depositors. It's a halt, never a theft, and every kill is announced on the vault's page and a public feed, with its reason.

## Closing

However a vault stops (retired, frozen or killed), it **closes** once a valuation after the halt finds its stations empty, and what that valuation priced has been settled. Anyone can send `close`. The fees owed are paid one last time, and then they stop: from here on the vault is worth exactly its cash.

- **Holders are paid from that cash,** each their share, whenever they like (`redeemClosed`), with no fee.
- **Requests made after the last valuation** are settled at the closed vault's price (`settleClosed`): a withdrawal is paid like any other holder, a deposit is sent back, and a tank fill lapses.
- **Claims still unpaid** are paid the same way (`claimClosed`).

Anyone may send `settleClosed` or `claimClosed`, so nobody's payout waits on its owner. Once a valuation has found a halted vault's stations empty, it takes no new request, so nobody can hold its close off a slot at a time.

## What the chain verifies

| Fact | Checked by the chain | Rests on |
| --- | --- | --- |
| Shares, deposits, withdrawals, claims and how they settled | ✓ | |
| That every depositor signed the vault's statement | ✓ | The depositor's own word: nobody's identity is checked |
| Fees, the high-water mark, the tank and what it was charged | ✓ | Charges are capped at the Author's allowances; within that, the executor's signed total |
| Cash in the vault, and what a station on its own chain holds | ✓ | The prices the station uses |
| What a station on another chain holds | Signed by a key blessed for this vault, and fresh | The attested machine that carried it |
| Which keys may trade for the vault, and what they may run | ✓ | Attestation, through each key's [blessing](attestation.md#blessing-a-key) |

[Next: stations →](stations.md){ .md-button .md-button--primary }
