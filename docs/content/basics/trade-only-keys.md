# Trade-only keys

An agent has a network connection and a model of its own, and it trades other people's money. So what stops it walking off with the lot?

<div class="shonin-hero" markdown="0">
<svg viewBox="0 0 1000 380" role="img" aria-labelledby="keys-title keys-desc" xmlns="http://www.w3.org/2000/svg">
<title id="keys-title">A trade-only key</title>
<desc id="keys-desc">A little key with a happy face stands on the left, wearing a tag that says trade only, rising out of the chip it was made in: the enclave inside the attested machine. Two lines branch out to its right. The upper one is a teal cord to a trading screen, with a green check: it can trade. The lower one, to a money bag, is red and crossed with an X: it cannot withdraw.</desc>
<ellipse cx="500" cy="190" rx="490" ry="188" style="fill:var(--h-blob)"/>
<!-- two branches: trading is joined, withdrawing is cut -->
<path d="M300 180 C420 180 470 112 620 112" style="fill:none;stroke:var(--h-teal);stroke-width:5;stroke-linecap:round"/>
<circle cx="620" cy="112" r="7" style="fill:var(--h-teal)"/>
<path d="M300 200 C420 200 470 272 620 272" style="fill:none;stroke:#dc2626;stroke-width:5;stroke-linecap:round;stroke-dasharray:12 10"/>
<circle cx="458" cy="238" r="19" style="fill:var(--h-paper);stroke:#dc2626;stroke-width:4"/>
<path d="M450 230 l16 16 M466 230 l-16 16" style="stroke:#dc2626;stroke-width:4.5;stroke-linecap:round"/>
<!-- the key, and the enclave it was made in -->
<g transform="translate(-250 -8)">
<rect x="491" y="160" width="18" height="132" rx="6" style="fill:var(--h-amber);stroke:var(--h-coin-ink);stroke-width:3"/>
<rect x="507" y="238" width="18" height="13" rx="2" style="fill:var(--h-amber);stroke:var(--h-coin-ink);stroke-width:3"/>
<rect x="507" y="264" width="24" height="16" rx="2" style="fill:var(--h-amber);stroke:var(--h-coin-ink);stroke-width:3"/>
<rect x="494" y="236" width="12" height="46" style="fill:var(--h-amber)"/>
<circle cx="500" cy="120" r="44" style="fill:var(--h-amber);stroke:var(--h-coin-ink);stroke-width:3"/>
<rect x="494" y="158" width="12" height="10" style="fill:var(--h-amber)"/>
<circle cx="482" cy="112" r="6" style="fill:var(--h-coin-ink)"/>
<circle cx="510" cy="112" r="6" style="fill:var(--h-coin-ink)"/>
<circle cx="480" cy="110" r="2" style="fill:#ffffff"/>
<circle cx="508" cy="110" r="2" style="fill:#ffffff"/>
<circle cx="474" cy="128" r="5" style="fill:#f87171;opacity:.45"/>
<circle cx="526" cy="128" r="5" style="fill:#f87171;opacity:.45"/>
<path d="M484 129 Q496 140 508 129" style="fill:none;stroke:var(--h-coin-ink);stroke-width:3;stroke-linecap:round"/>
<path d="M470 270 v-8 M530 270 v-8 M452 294 h-8 M452 310 h-8 M452 326 h-8 M548 294 h8 M548 310 h8 M548 326 h8 M470 340 v8 M500 340 v8 M530 340 v8" style="stroke:var(--h-ink);stroke-width:3;stroke-linecap:round"/>
<rect x="452" y="270" width="96" height="70" rx="12" style="fill:var(--h-indigo)"/>
<rect x="464" y="282" width="72" height="46" rx="8" style="fill:none;stroke:var(--h-paper);stroke-width:2;opacity:.45"/>
<path d="M478 258 l-10 -8 M522 258 l10 -8 M486 250 l-5 -12 M514 250 l5 -12" style="stroke:var(--h-amber);stroke-width:3;stroke-linecap:round"/>
</g>
<!-- its tag -->
<path d="M242 196 Q228 200 212 206" style="fill:none;stroke:var(--h-ink);stroke-width:2"/>
<g transform="rotate(-6 150 214)">
<rect x="88" y="195" width="124" height="38" rx="7" style="fill:var(--h-paper);stroke:var(--h-ink);stroke-width:2.5"/>
<circle cx="199" cy="214" r="4" style="fill:none;stroke:var(--h-ink);stroke-width:2"/>
<text x="142" y="220" class="h-tag">trade only</text>
</g>
<!-- can trade -->
<circle cx="700" cy="112" r="70" style="fill:var(--h-glass-disc)"/>
<g transform="translate(500 -63)">
<rect x="148" y="140" width="104" height="72" rx="8" style="fill:var(--h-ink)"/>
<rect x="155" y="147" width="90" height="58" rx="4" style="fill:var(--h-screen)"/>
<path d="M176 156 v40 M200 162 v38 M224 152 v36" style="stroke:var(--h-muted);stroke-width:1.5"/>
<rect x="171" y="166" width="10" height="20" rx="2" style="fill:#2dd4bf"/>
<rect x="195" y="172" width="10" height="20" rx="2" style="fill:#f87171"/>
<rect x="219" y="160" width="10" height="20" rx="2" style="fill:#2dd4bf"/>
</g>
<circle cx="752" cy="62" r="18" style="fill:var(--h-teal);stroke:var(--h-paper);stroke-width:3"/>
<path d="M744 62 l5 5 l11 -12" style="fill:none;stroke:var(--h-badge-ink);stroke-width:3.5;stroke-linecap:round;stroke-linejoin:round"/>
<!-- can't withdraw -->
<circle cx="700" cy="272" r="70" style="fill:var(--h-red-soft)"/>
<g transform="translate(-100 90)">
<rect x="788" y="142" width="24" height="18" rx="5" style="fill:var(--h-amber);stroke:var(--h-coin-ink);stroke-width:2.5"/>
<path d="M776 166 C760 206 780 222 800 222 C820 222 840 206 824 166 Z" style="fill:var(--h-amber);stroke:var(--h-coin-ink);stroke-width:2.5;stroke-linejoin:round"/>
<rect x="782" y="158" width="36" height="8" rx="4" style="fill:var(--h-coin-ink)"/>
<text x="800" y="204" class="h-bag">$</text>
</g>
<circle cx="752" cy="222" r="18" style="fill:#dc2626;stroke:var(--h-paper);stroke-width:3"/>
<path d="M746 216 l12 12 M758 216 l-12 12" style="stroke:#ffffff;stroke-width:3.5;stroke-linecap:round"/>
<text x="870" y="119" class="h-caption">can trade</text>
<text x="870" y="279" class="h-caption">can't withdraw</text>
<text x="250" y="372" class="h-caption">made in the enclave</text>
</svg>
</div>

The agent never holds a key. It decides what to trade, and the attested machine signs the order with the vault's key. That key is **trade-only**: the exchange lets it place and cancel orders, and nothing else. It can't withdraw, can't send money to another address, and can't hand its powers to anyone else.

Moving money at all goes through our [smart contracts](smart-contracts.md), which only move it between accounts the vault owns.

So nobody can withdraw a Depositor's money: not the agent, not its Author, not Shonin, and not somebody who has stolen all our keys. The exchange and our contracts refuse it, whatever the code says.

That's the trust bar: *halt, never withdraw*. An agent can be switched off. The money can't be taken out.

!!! warning "Trade-only doesn't mean risk-free"
    A key that can trade can also lose money, and it could lose it on purpose. An agent could keep buying from an account its Author controls at too high a price, moving value out one bad trade at a time, without ever withdrawing anything.

    What helps Depositors judge that risk is the Reviewer's report, the agent's public source and its track record, and what protects them is that they can leave at any time. The price of a trade is only limited on-chain when a withdrawal has to close positions.

[Next: smart contracts →](smart-contracts.md){ .md-button .md-button--primary }
