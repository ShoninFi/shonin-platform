# Depositors

As a Depositor, you put money into an agent's vault and get shares back. Your shares rise and fall with the vault, and you can cash them in whenever you like.

!!! tip "New here?"
    The [System Overview](basics/trading-agents.md) explains how it all fits together, and the [Glossary](reference/glossary.md) explains the terms used here.

<div class="shonin-hero" markdown="0">
<svg viewBox="0 0 1000 360" role="img" aria-labelledby="deposit-title deposit-desc" xmlns="http://www.w3.org/2000/svg">
<title id="deposit-title">Money in, shares back</title>
<desc id="deposit-desc">A Depositor tosses a coin up into the slot on top of a vault, and a ticket marked share slides back out of the vault towards their other hand, which holds a little stack of coins: the money they can take back out.</desc>
<ellipse cx="500" cy="196" rx="480" ry="150" style="fill:var(--h-blob)"/>
<ellipse cx="300" cy="322" rx="48" ry="8" style="fill:var(--h-shadow)"/>
<ellipse cx="715" cy="322" rx="110" ry="9" style="fill:var(--h-shadow)"/>
<!-- the vault -->
<rect x="620" y="130" width="190" height="186" rx="18" style="fill:var(--h-indigo-soft);stroke:var(--h-indigo);stroke-width:3"/>
<rect x="636" y="146" width="158" height="154" rx="12" style="fill:var(--h-paper);stroke:var(--h-indigo);stroke-width:2"/>
<rect x="626" y="172" width="8" height="24" rx="3" style="fill:var(--h-indigo)"/>
<circle cx="715" cy="223" r="36" style="fill:var(--h-indigo-soft);stroke:var(--h-indigo);stroke-width:3"/>
<path d="M715 197 v-9 M715 249 v9 M689 223 h-9 M741 223 h9" style="stroke:var(--h-indigo);stroke-width:4;stroke-linecap:round"/>
<circle cx="715" cy="223" r="11" style="fill:var(--h-indigo)"/>
<rect x="695" y="122" width="40" height="9" rx="3" style="fill:var(--h-slot)"/>
<rect x="614" y="270" width="10" height="34" rx="3" style="fill:var(--h-slot)"/>
<!-- money in -->
<path d="M366 170 Q520 50 704 116" style="fill:none;stroke:var(--h-muted);stroke-width:2.5;stroke-linecap:round;stroke-dasharray:1 8"/>
<circle cx="530" cy="104" r="12" style="fill:var(--h-amber);stroke:var(--h-coin-ink);stroke-width:1.5"/>
<text x="530" y="109" class="h-coin">$</text>
<!-- shares back -->
<path d="M610 286 Q480 304 392 282" style="fill:none;stroke:var(--h-teal);stroke-width:2.5;stroke-linecap:round;stroke-dasharray:1 8"/>
<g transform="rotate(-5 500 292)">
<rect x="458" y="269" width="88" height="46" rx="7" style="fill:var(--h-paper);stroke:var(--h-teal);stroke-width:2.5"/>
<circle cx="458" cy="292" r="7" style="fill:var(--h-blob-solid)"/>
<circle cx="546" cy="292" r="7" style="fill:var(--h-blob-solid)"/>
<text x="502" y="289" class="h-ticket">share</text>
<polyline points="478,304 490,298 500,301 522,292" style="fill:none;stroke:var(--h-teal);stroke-width:2.5;stroke-linecap:round;stroke-linejoin:round"/>
</g>
<!-- the Depositor -->
<rect x="289" y="312" width="11" height="10" rx="4" style="fill:var(--h-hair)"/>
<rect x="289" y="276" width="11" height="44" rx="5" style="fill:var(--h-hair)"/>
<rect x="305" y="276" width="11" height="44" rx="5" style="fill:var(--h-hair)"/>
<path d="M268 282 Q268 208 302 204 Q336 208 336 282 Z" style="fill:var(--h-teal)"/>
<path d="M326 226 Q340 212 346 196" style="fill:none;stroke:var(--h-teal);stroke-width:9;stroke-linecap:round"/>
<circle cx="347" cy="192" r="6" style="fill:var(--h-skin)"/>
<circle cx="356" cy="178" r="12" style="fill:var(--h-amber);stroke:var(--h-coin-ink);stroke-width:1.5"/>
<text x="356" y="183" class="h-coin">$</text>
<path d="M330 250 Q344 262 352 274" style="fill:none;stroke:var(--h-teal);stroke-width:9;stroke-linecap:round"/>
<circle cx="354" cy="278" r="6" style="fill:var(--h-skin)"/>
<!-- money back: a little stack of coins in the other hand -->
<path d="M356 285 v9 a15 5 0 0 0 30 0 v-9" style="fill:var(--h-amber-dark);stroke:var(--h-coin-ink);stroke-width:1.5;stroke-linejoin:round"/>
<ellipse cx="371" cy="285" rx="15" ry="5" style="fill:var(--h-amber);stroke:var(--h-coin-ink);stroke-width:1.5"/>
<path d="M359 275 v9 a15 5 0 0 0 30 0 v-9" style="fill:var(--h-amber-dark);stroke:var(--h-coin-ink);stroke-width:1.5;stroke-linejoin:round"/>
<ellipse cx="374" cy="275" rx="15" ry="5" style="fill:var(--h-amber);stroke:var(--h-coin-ink);stroke-width:1.5"/>
<circle cx="302" cy="178" r="24" style="fill:var(--h-skin)"/>
<path d="M278 176 Q279 152 302 152 Q327 152 326 174 Q316 165 302 166 Q288 167 278 176 Z" style="fill:var(--h-hair)"/>
<text x="530" y="72" class="h-caption">money in</text>
<text x="302" y="352" class="h-caption">Depositor</text>
<text x="500" y="352" class="h-caption">shares back</text>
<text x="715" y="352" class="h-caption">vault</text>
</svg>
</div>

## Choosing an agent

Which agent to back is up to you. Every agent's page gives you what you need to make an informed choice.

### The agent

- **The review.** The [AI Review](basics/ai-review.md) report card: six areas rated, and a summary of what the code does.
- **Source code (if shared).** You can read it, and check it's the code actually trading.

### Its history

- **Share price over time.** How the price of one share has moved since the vault opened, with its [high-water mark](fees.md#the-profit-fee) (the highest it's been on a payday, which the profit fee is measured from), read straight from the vault's contract. Look at it by the day, the week or the month. A young agent is marked **new**, because there's less history to go on.
- **The money in its vault over time.** How much was in the vault along the way. It's a different thing from the share price: money coming and going changes it, but only the agent's trading moves the price. A strategy that works with a small pot often doesn't with a big one.

### Its vault

- **Where it trades.** The venues (exchanges) and bridges it uses, and how each venue keeps your money safe. [Smart contracts](basics/smart-contracts.md) explains why that matters.
- **Its tank.** The Author's account that pays for running the agent, kept apart from your money. How full it is matters, because an agent whose [tank](tank.md) runs dry stops trading.
- **How you'd get out.** Whether the contract can always finish a withdrawal by itself, or if it relies on additional operations. [Getting out](#getting-out), below, has the details.

## Getting out

You can ask for your money back at any time. It goes one of two ways:

**Paid straight away.** The vault keeps some cash at home, in the contract itself. If there's enough to cover your withdrawal, you're paid in the same transaction.

**Booked.** If there isn't enough cash at home, your withdrawal is booked. The contract closes your share of each of the vault's positions, and pays you once that cash is back. Anyone can push this through, including you, so it doesn't depend on us.

A few vaults trade somewhere the contract can't reach by itself, and rely on an operator to finish a booked withdrawal. Their page says so.

**What it costs.** Your shares are valued at the prices the vault could actually sell at, not the middle of the market, and there's the 0.1% [withdrawal fee](fees.md#the-deposit-and-withdrawal-fee), which stays in the vault for the people still in it.

## What can still go wrong

**The agent can lose money.** Nothing here stops that, and it could even lose it on purpose, as [Trade-only keys](basics/trade-only-keys.md) explains. A review isn't a prediction, and a track record isn't a promise.

**The agent can stop.** If its tank runs dry, it stops until someone tops it up, and its open positions sit there with nothing managing them. You can still leave whenever you like.

**A withdrawal can be delayed.** If a booked withdrawal still isn't paid after seven days, anyone can freeze the vault. It stops taking deposits, so nobody new joins a vault that can't pay out, and from then on it can only wind down.

**The agent can be retired.** Its Author can retire it at any time. The vault then stops taking deposits and gives everyone a week to leave as usual, after which the agent stops for good and its positions are closed at market, which can lock in a loss.

**The vault can be stopped.** We can stop a vault that keeps failing. That closes every position at market, which can lock in a loss, and brings the cash home so everyone can withdraw.

**A venue can fail.** Your money is only as safe as the venues holding it.

What can't happen is someone withdrawing your money or sending it somewhere else. There's no way to do it.

[Next: authors →](authors.md){ .md-button .md-button--primary }
