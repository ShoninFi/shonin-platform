# The tank

Running an agent costs money. Each vault has a **tank**, the Author's account for running it: the Author fills it, their earnings can top it up, and the running costs come out of it. They never come out of the Depositors' money.

<div class="shonin-hero" markdown="0">
<svg viewBox="0 0 1000 380" role="img" aria-labelledby="tank-title tank-desc" xmlns="http://www.w3.org/2000/svg">
<title id="tank-title">The tank's cycle</title>
<desc id="tank-desc">A friendly fuel tank with a face and a gauge sits on the left, and a loop of arrows runs around the picture. From the tank, money flows to the chip of the attested machine: it pays to run the agent. From the chip, a teal arrow carrying a green coin runs down to the vault: the agent trades and makes a profit. From the vault, auto-fill tops the tank up from the Author's earnings. Off to one side, a single coin drops into the tank: the Author's first fill.</desc>
<ellipse cx="500" cy="192" rx="485" ry="182" style="fill:var(--h-blob)"/>
<!-- the tank -->
<g transform="translate(-250 22)">
<rect x="482" y="74" width="36" height="22" rx="5" style="fill:var(--h-ink)"/>
<rect x="430" y="92" width="140" height="180" rx="38" style="fill:var(--h-amber-soft);stroke:var(--h-ink);stroke-width:3"/>
<path d="M433 178 H567 V234 a38 38 0 0 1 -38 38 H471 a38 38 0 0 1 -38 -38 Z" style="fill:var(--h-amber)"/>
<rect x="430" y="92" width="140" height="180" rx="38" style="fill:none;stroke:var(--h-ink);stroke-width:3"/>
<path d="M430 140 h12 M430 178 h16 M430 216 h12" style="stroke:var(--h-ink);stroke-width:2.5;stroke-linecap:round"/>
<circle cx="480" cy="130" r="6" style="fill:var(--h-ink)"/>
<circle cx="520" cy="130" r="6" style="fill:var(--h-ink)"/>
<circle cx="478" cy="128" r="2" style="fill:var(--h-paper)"/>
<circle cx="518" cy="128" r="2" style="fill:var(--h-paper)"/>
<circle cx="468" cy="148" r="6" style="fill:#f87171;opacity:.45"/>
<circle cx="532" cy="148" r="6" style="fill:#f87171;opacity:.45"/>
<path d="M486 148 Q500 160 514 148" style="fill:none;stroke:var(--h-ink);stroke-width:3;stroke-linecap:round"/>
<!-- its gauge, half full -->
<circle cx="500" cy="226" r="26" style="fill:#ffffff;stroke:var(--h-ink);stroke-width:3"/>
<path d="M480 232 a20 20 0 0 1 40 0" style="fill:none;stroke:var(--h-muted);stroke-width:3;stroke-linecap:round"/>
<path d="M480 232 a20 20 0 0 1 20 -20" style="fill:none;stroke:var(--h-teal);stroke-width:3;stroke-linecap:round"/>
<path d="M500 232 L497 214" style="stroke:#1e1b4b;stroke-width:3;stroke-linecap:round"/>
<circle cx="500" cy="232" r="4" style="fill:#1e1b4b"/>
</g>
<!-- the Author's first fill, once -->
<circle cx="92" cy="96" r="16" style="fill:var(--h-amber);stroke:var(--h-coin-ink);stroke-width:2.5"/>
<text x="92" y="102" class="h-coin" style="font-size:16px">$</text>
<path d="M110 106 C140 116 158 126 172 140" style="fill:none;stroke:var(--h-amber-dark);stroke-width:3.5;stroke-linecap:round;stroke-dasharray:2 8"/>
<path d="M158.5 138.5 L174 142 L172.0 126.3" style="fill:none;stroke:var(--h-amber-dark);stroke-width:3.5;stroke-linecap:round;stroke-linejoin:round"/>
<text x="92" y="62" class="h-tag">first fill</text>
<!-- the tank pays to run the agent -->
<path d="M322 132 C420 50 600 44 684 92" style="fill:none;stroke:var(--h-amber-dark);stroke-width:4;stroke-linecap:round;stroke-dasharray:2 9"/>
<path d="M670.2 94.7 L686 93 L679.0 78.8" style="fill:none;stroke:var(--h-amber-dark);stroke-width:4;stroke-linecap:round;stroke-linejoin:round"/>
<text x="505" y="52" class="h-tag">pays to run it</text>
<circle cx="760" cy="112" r="72" style="fill:var(--h-glass-disc)"/>
<g transform="translate(760 132) translate(-500 -305)">
<path d="M470 270 v-8 M530 270 v-8 M452 294 h-8 M452 310 h-8 M452 326 h-8 M548 294 h8 M548 310 h8 M548 326 h8 M470 340 v8 M500 340 v8 M530 340 v8" style="stroke:var(--h-ink);stroke-width:3;stroke-linecap:round"/>
<rect x="452" y="270" width="96" height="70" rx="12" style="fill:var(--h-indigo)"/>
<rect x="464" y="282" width="72" height="46" rx="8" style="fill:none;stroke:var(--h-paper);stroke-width:2;opacity:.45"/>
<path d="M478 258 l-10 -8 M522 258 l10 -8 M486 250 l-5 -12 M514 250 l5 -12" style="stroke:var(--h-amber);stroke-width:3;stroke-linecap:round"/>
</g>
<!-- the agent trades, and the vault makes a profit -->
<path d="M840 126 C888 170 888 230 842 268" style="fill:none;stroke:var(--h-teal);stroke-width:4;stroke-linecap:round"/>
<path d="M846.0 252.6 L842 268 L857.8 266.5" style="fill:none;stroke:var(--h-teal);stroke-width:4;stroke-linecap:round;stroke-linejoin:round"/>
<circle cx="876" cy="199" r="16" style="fill:var(--h-teal);stroke:var(--h-paper);stroke-width:3"/>
<text x="876" y="205" class="h-coin" style="font-size:16px;fill:var(--h-badge-ink)">$</text>
<text x="936" y="205" class="h-tag">profit</text>
<circle cx="760" cy="282" r="72" style="fill:var(--h-disc)"/>
<g transform="translate(756 284) scale(.42) translate(-695 -227)">
<rect x="600" y="132" width="190" height="190" rx="18" style="fill:var(--h-indigo-soft);stroke:var(--h-indigo);stroke-width:3"/>
<rect x="616" y="148" width="158" height="158" rx="12" style="fill:var(--h-paper);stroke:var(--h-indigo);stroke-width:2"/>
<rect x="606" y="174" width="8" height="24" rx="3" style="fill:var(--h-indigo)"/>
<rect x="606" y="256" width="8" height="24" rx="3" style="fill:var(--h-indigo)"/>
<circle cx="695" cy="227" r="38" style="fill:var(--h-indigo-soft);stroke:var(--h-indigo);stroke-width:3"/>
<path d="M695 201 v-10 M695 253 v10 M669 227 h-10 M721 227 h10" style="stroke:var(--h-indigo);stroke-width:4;stroke-linecap:round"/>
<circle cx="695" cy="227" r="11" style="fill:var(--h-indigo)"/>
</g>
<!-- the Author's earnings top it up: auto-fill -->
<path d="M686 306 C600 372 420 372 334 300" style="fill:none;stroke:var(--h-amber-dark);stroke-width:4;stroke-linecap:round;stroke-dasharray:2 9"/>
<path d="M347.8 299.0 L332 298 L336.5 313.2" style="fill:none;stroke:var(--h-amber-dark);stroke-width:4;stroke-linecap:round;stroke-linejoin:round"/>
<text x="505" y="322" class="h-tag">auto-fill from earnings</text>
</svg>
</div>

## What it pays for

The tank pays for two things:

**Resources.** What the agent consumes as it runs: processor time, memory, network traffic, local inference, etc. Our prices for each are published in US dollars.

**Gas.** The fee a blockchain charges for each transaction the attested machine sends for the agent, starting with the ones that set up its vault. It's set by the chain, not by us, so it depends on where the agent trades: Hyperliquid charges no gas for orders, while on other chains, like Ethereum, gas can be costly and rises when the network is busy.

We cover both as they happen, and set the money aside in the tank: each cost goes on a running tab, and once a day we're paid the tab out of the tank. The tab can't get more than a few seconds ahead of what's in the tank, even for an agent written to waste money on purpose:

- **Gas is checked before it's spent.** Like any wallet, the attested machine sets a transaction's maximum gas fee before sending it, and adds that to the tab. If the tank can't cover it, the transaction isn't sent and the agent stops.
- **Resources are counted as they're used,** a few seconds' worth at a time, since there's no knowing in advance how much work the agent will do. The moment the tab reaches what's in the tank, the agent stops, so at most a few seconds' worth can slip past, and we absorb that.

Every vault's page shows how full its tank is and its recent charges. The Author sets limits on both resources and gas, and the vault's contract never pays out more than those limits allow.

## What it doesn't pay for

**Private services.** If the agent uses a paid third-party service, like a data feed or an AI model hosted somewhere else, the Author brings their own account. They seal its API key into the agent, where nobody can read it, and the service bills the Author directly.

**Trading fees.** The fees a venue charges on each trade, like Hyperliquid's trading fee, come out of the trade itself, from the vault's trading account. They're part of what the agent makes or loses, as at any fund.

**Leaving the vault.** A withdrawal pays its own way: a Depositor pays its network fee, like any other transaction. On the few vaults that need an operator to finish an exit, the operator's costs come out of that withdrawal.

## Filling it

When an Author publishes, they pay the $1 to publish, which covers the review and the build, and whatever they'd like in the tank. Past the $1 it's up to them, but an agent with an empty tank stops, so it's wise to add about a week of running.

After that, a successful agent can pay its own way. The Author can top the tank up from their earnings with one click, or switch on **auto-fill** and pick a number, "keep my tank at $X", and it tops itself up every day. The [Money flow](flow.md) page shows how that plays out.

## Taking money out

The Author can take money out whenever they like, say if they've put in more than they need: the tab is paid first, the rest goes to their wallet, and whatever they leave in keeps the agent running.

## If it runs dry

The agent stops the moment the tab reaches what's in the tank, whether the tank ran low or the Author emptied it. Anyone can top it up to start the agent again.

We don't step in beyond that. An agent its Author stops paying for is simply an unsuccessful one: its open positions stay where they are, with nothing managing them, and the vault's page says so loudly. An empty tank adds no risk beyond the agent's own trades, and Depositors can still leave whenever they like.

The [Vaults](how-it-works/vaults.md#the-tank) page in Hands-On has the details.

[Next: fees →](fees.md){ .md-button .md-button--primary }
