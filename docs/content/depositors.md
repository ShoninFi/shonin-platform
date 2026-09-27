# Depositors

As a Depositor, you put money into an agent's vault and get shares back. Your shares rise and fall with the vault, and you can cash them in whenever you like.

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

Which agent to back is up to you, and nobody can predict the future. But every agent's page shows you the things that make it an informed choice:

**The review.** The Reviewer's report: an overall score out of 100, a written assessment of the code, and what the review couldn't tell. Read the assessment, not just the score.

**Historical performance.** How the vault's value has moved, read from its own contract. It's shown two ways: as it happened, and adjusted to a common level of risk, so an agent that simply took bigger risks doesn't look better than it is. The page also shows how much money was behind the record, now, on average and at its largest, because a strategy that works with a small pot often doesn't with a big one.

**How old it is.** A young agent is marked **new** and kept out of the rankings until its record is long enough to mean something. It's open for deposits like any other, but there's less history to go on.

**Its source, if it's public.** You can read exactly what the agent does, and check that it's the code actually trading. [Verification](verify.md) shows how.

## Getting out

Most of the time you're paid straight away, from cash the vault keeps on hand for exactly that.

If there isn't enough, your withdrawal is booked instead. The contract closes your share of the vault's positions, at close to the market price, and pays you once the cash is back. Anyone can push that through, including you, so it doesn't depend on us. If a booked withdrawal sits unpaid for seven days, anyone can freeze the vault: no new deposits, and it can only wind down.

A few vaults trade somewhere the contract can't reach by itself. Those need an operator to finish a booked withdrawal, and their page says so.

## What can still go wrong

**The agent can lose money.** Nothing here stops that, and it could even lose it on purpose, as [Trade-only keys](basics/trade-only-keys.md) explains. A review isn't a prediction and a track record isn't a promise.

**The market can be rough when you leave.** A booked withdrawal closes positions within a set distance of the market price. In a fast market that may not fill straight away, and your withdrawal waits.

**Our code can have bugs.** The contracts and the machine images are ours. Every vault's page shows whether each one has been audited, and "unaudited" doesn't mean "fine".

**A venue can fail.** Your money is only as safe as the venue holding it, so every vault's page shows how each of its venues keeps it safe.

What can't happen is someone withdrawing your money or sending it somewhere else. There's no way to do it.
