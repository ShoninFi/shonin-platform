# Vaults

A vault is one contract holding one pool of capital behind one strategy version. It computes its own value, mints and burns its own shares, pays its own withdrawals, and charges its own fees. Nobody reports anything to it.

## Shares

Shares follow **ERC-4626**, the standard interface for tokenised vaults: the vault is an ERC-20 whose shares are a pro-rata claim on a pool of one asset, with a share price of assets divided by shares that wallets and other contracts already understand.

A deposit mints shares at the vault's current value per share. Your profit and loss is your shares times today's value per share, less what you paid.

The standard's one fairness rule — round against whoever is moving — is applied to prices as well as to shares:

| Moving | Valued at |
| --- | --- |
| Depositing | Mark |
| Withdrawing | Exit-side marks: a long at the best bid, a short at the best ask |

So the mover pays the spread, and the pool never does.

!!! note "Not fully ERC-4626 compliant"
    `redeem` may book a claim rather than transfer assets, in the manner of ERC-7540. An integrator should expect that.

## Leaving

```mermaid
flowchart TB
    R(["redeem(shares)"]) --> C["crystallise any fees due"]
    C --> V["value at exit-side marks,<br/>take the transaction fee"]
    V --> Q{"does the idle balance<br/>cover it?"}
    Q ==>|yes| P(["burn the shares,<br/>pay in this transaction"])
    Q -->|no| B["mark the shares pending,<br/>book a claim"]
    B --> S["<b>settle</b> — anyone may call it<br/>close your fraction of every position,<br/>inside the exit band"]
    S --> H["cash home: value again, burn<br/>at the marks then, pay"]
    H --> P2(["paid"])

    classDef fast fill:#0d948818,stroke:#0d9488,color:inherit
    classDef slow fill:#f59e0b18,stroke:#f59e0b,color:inherit
    classDef ends fill:#4338ca18,stroke:#4338ca,color:inherit,font-weight:bold
    class C,V fast
    class B,S,H slow
    class R,P,P2 ends
```

Strategies keep a working buffer of idle capital, so most withdrawals end in the first branch: the shares are burned and the depositor is paid in the same transaction.

When idle capital cannot cover it, the shares are marked pending and the contract books a **claim**, which has three states — *pending*, *waiting* when a settlement was tried and the market was outside the band, and *paid*.

`settle` is a public function, and that is the part worth dwelling on. A depositor does not need us, or the author, or the executor, to get their money. If every service we run stopped this minute, they could send the settle transaction themselves and be paid by the contract.

### The exit band

A claim's positions are closed with reduce-only immediate-or-cancel orders, limited to the venue's oracle price plus or minus the **exit band**, 1% by default. The pool's cost beyond the spread is therefore bounded by that band times the notional closed. If the market is outside the band the close does not fill, the claim waits, and settlement is retried.

The leaver pays the spread through exit-side marks, pays the transaction fee, and bears the close's impact pro rata with everyone else, because their shares are still outstanding when it lands. The depositors who stay keep the transaction fee.

A claim that goes unserved for seven days lets anyone freeze the vault: no new deposits, and only unwinding left.

## Fees

Fees crystallise daily as **new shares**, so paying them never forces a sale.

```text
daily, anyone can call crystallise():

  streaming fee  = value × 0.5% × (days since last) / 365      → the platform
  if value per share (after streaming) > high-water mark:
      gains      = (value per share − high-water mark) × shares
      author fee = gains × 10%                                  → the author
      platform   = gains × 5%                                   → the platform
      high-water mark = the new value per share

on every deposit and withdrawal:
  transaction fee = 0.1% of the amount                          → stays in the pool
```

- **The high-water mark is the vault's own.** It is the highest value per share the vault has ever crystallised at, so profit fees are only taken on new highs. A loss followed by a recovery pays nothing on the recovery.
- **The transaction fee is nobody's.** It stays in the pool and raises value per share for the depositors who stay, so timing a deposit or a withdrawal around a price update cannot take value from them.
- **Moving to a new version** costs the transaction fee out and in, and the new vault's high-water mark starts fresh.

The schedule is fixed when a vault is created and never changes under it. A vault's page shows its own schedule, not today's default.

## The tank

Running a strategy costs money: compute, and gas where a chain charges it. That is the **author's** cost, not the depositors'.

Each vault has a **tank**, which the author funds when they create it and their own fees refill. It is excluded from what shares are worth, so a depositor's value never pays for the strategy's electricity.

- The build and the review are the tank's first charges.
- The executor states each day's usage, signed by the vault's own blessed key, and the contract believes it only up to the allowances the author declared at the platform's published prices. A wrong statement cannot charge more than the author agreed to.
- **Auto-fill** redeems a share of each author fee back into the tank — all of it, by default — until the tank holds its target, around thirty days of charges. It is paid from idle balance and waits for the next day when there is not enough, never booking a claim.
- A tank that runs dry accrues a debt, repaid from future author fees before anything reaches the author.

Every vault page shows the tank's balance, what it is charging and how long that lasts.

## A kill

Shonin's admin key can **kill** any vault, typically after its health report shows it failing. A kill closes every position through the same reduce-only path a claim uses and brings all the cash home. After that the vault holds only idle balance, so every depositor's withdrawal is paid in one transaction.

It moves no funds anywhere but back to depositors. It is a halt, never a theft. Every kill is announced on the vault's page and on a public feed, with its reason.

## What the chain verifies

| Fact | On the chain |
| --- | --- |
| Share supply, deposits, withdrawals, pending shares, claims and their settlement | ✓ |
| Fees, the high-water mark, the tank and what it was charged | ✓ |
| The vault's value, computed by the contract from venue state | ✓ |
| Which keys may act for the vault, and what they may run | ✓ |
| Every order and fill on the venue | ✓ |
