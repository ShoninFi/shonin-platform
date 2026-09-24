# Trade-only keys

An agent has a network connection and a model of its own, and it trades other people's money. So what stops it walking off with the lot?

<div class="shonin-hero" markdown="0">
<svg viewBox="0 0 1000 380" role="img" aria-labelledby="keys-title keys-desc" xmlns="http://www.w3.org/2000/svg">
<title id="keys-title">A trade-only key</title>
<desc id="keys-desc">A little key with a happy face stands in the middle, wearing a tag that says trade only, rising out of the chip it was made in: the enclave inside the attested machine. A teal cord joins it to a trading screen on its left, with a green check: it can trade. On its right, with nothing joining them, a money bag inside a red no sign: it cannot withdraw.</desc>
<ellipse cx="500" cy="175" rx="480" ry="150" style="fill:var(--h-blob)"/>
<!-- can trade -->
<circle cx="200" cy="175" r="80" style="fill:var(--h-glass-disc)"/>
<rect x="148" y="140" width="104" height="72" rx="8" style="fill:var(--h-ink)"/>
<rect x="155" y="147" width="90" height="58" rx="4" style="fill:var(--h-screen)"/>
<path d="M176 156 v40 M200 162 v38 M224 152 v36" style="stroke:var(--h-muted);stroke-width:1.5"/>
<rect x="171" y="166" width="10" height="20" rx="2" style="fill:#2dd4bf"/>
<rect x="195" y="172" width="10" height="20" rx="2" style="fill:#f87171"/>
<rect x="219" y="160" width="10" height="20" rx="2" style="fill:#2dd4bf"/>
<circle cx="262" cy="118" r="20" style="fill:var(--h-teal);stroke:var(--h-paper);stroke-width:3"/>
<path d="M253 118 l6 6 l12 -13" style="fill:none;stroke:var(--h-badge-ink);stroke-width:3.5;stroke-linecap:round;stroke-linejoin:round"/>
<!-- the key is joined to trading, and to nothing else -->
<path d="M457 128 C400 150 340 176 282 176" style="fill:none;stroke:var(--h-teal);stroke-width:5;stroke-linecap:round"/>
<circle cx="282" cy="176" r="7" style="fill:var(--h-teal)"/>
<!-- the key -->
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
<!-- the enclave the key was made in -->
<path d="M470 270 v-8 M530 270 v-8 M452 294 h-8 M452 310 h-8 M452 326 h-8 M548 294 h8 M548 310 h8 M548 326 h8 M470 340 v8 M500 340 v8 M530 340 v8" style="stroke:var(--h-ink);stroke-width:3;stroke-linecap:round"/>
<rect x="452" y="270" width="96" height="70" rx="12" style="fill:var(--h-indigo)"/>
<rect x="464" y="282" width="72" height="46" rx="8" style="fill:none;stroke:var(--h-paper);stroke-width:2;opacity:.45"/>
<path d="M478 258 l-10 -8 M522 258 l10 -8 M486 250 l-5 -12 M514 250 l5 -12" style="stroke:var(--h-amber);stroke-width:3;stroke-linecap:round"/>
<!-- its tag -->
<path d="M541 108 Q562 114 570 138" style="fill:none;stroke:var(--h-ink);stroke-width:2"/>
<g transform="rotate(10 620 152)">
<rect x="560" y="134" width="124" height="38" rx="7" style="fill:var(--h-paper);stroke:var(--h-ink);stroke-width:2.5"/>
<circle cx="573" cy="153" r="4" style="fill:none;stroke:var(--h-ink);stroke-width:2"/>
<text x="628" y="159" class="h-tag">trade only</text>
</g>
<!-- can't withdraw -->
<circle cx="800" cy="175" r="80" style="fill:var(--h-red-soft)"/>
<rect x="788" y="142" width="24" height="18" rx="5" style="fill:var(--h-amber);stroke:var(--h-coin-ink);stroke-width:2.5"/>
<path d="M776 166 C760 206 780 222 800 222 C820 222 840 206 824 166 Z" style="fill:var(--h-amber);stroke:var(--h-coin-ink);stroke-width:2.5;stroke-linejoin:round"/>
<rect x="782" y="158" width="36" height="8" rx="4" style="fill:var(--h-coin-ink)"/>
<text x="800" y="204" class="h-bag">$</text>
<circle cx="800" cy="182" r="62" style="fill:none;stroke:#dc2626;stroke-width:9"/>
<path d="M756 138 L844 226" style="stroke:#dc2626;stroke-width:9;stroke-linecap:round"/>
<text x="200" y="322" class="h-caption">can trade</text>
<text x="500" y="372" class="h-caption">made in the enclave</text>
<text x="800" y="322" class="h-caption">can't withdraw</text>
</svg>
</div>

The agent never holds a key. It decides what to trade, and the attested machine signs the order with the vault's key. That key is **trade-only**: the venue lets it place and cancel orders, and nothing else. It can't withdraw, can't send money to another address, and can't hand its powers to anyone else.

Moving money at all goes through the vault's own contract, which only moves it between accounts the vault owns.

So nobody can withdraw a Depositor's money: not the agent, not its Author, not Shonin, and not somebody who has stolen all our keys. The venue and our contracts refuse it, whatever the code says.

That's the trust bar: *halt, never withdraw*. An agent can be switched off. The money can't be taken out.

!!! warning "Trade-only doesn't mean risk-free"
    A key that can trade can also lose money, and it could lose it on purpose. An agent could keep buying from an account its Author controls at too high a price, moving value out one bad trade at a time, without ever withdrawing anything.

    What protects Depositors from that is the Reviewer's report, the agent's public source, its track record, and the fact that they can leave at any time. The price of a trade is only limited on-chain when a withdrawal has to close positions.

[Don't trust us, verify it yourself →](../verify.md){ .md-button .md-button--primary }
