# Trading agents

A trading agent is a program an Author writes to trade the money Depositors put in. Because it runs on a real computer and not on a blockchain, it can follow anything online - from market prices to the news and social media. It decides what to do and then does it.

<div class="shonin-hero" markdown="0">
<svg viewBox="0 0 1000 350" role="img" aria-labelledby="sandbox-title sandbox-desc" xmlns="http://www.w3.org/2000/svg">
<title id="sandbox-title">An agent in its sandbox</title>
<desc id="sandbox-desc">A small robot, the agent, sits in a wooden sandbox with a sign that says sandbox. Around it in the sand are its tools: a screen of prices, buttons for buying and selling, an antenna for live connections, an AI chip, a notebook for its memory, and an envelope sealed with a padlock for its secrets.</desc>
<ellipse cx="500" cy="196" rx="480" ry="160" style="fill:var(--h-blob)"/>
<ellipse cx="500" cy="322" rx="370" ry="12" style="fill:var(--h-shadow)"/>
<!-- the sign -->
<rect x="112" y="178" width="9" height="142" rx="2" style="fill:var(--h-wood-dark)"/>
<rect x="58" y="146" width="118" height="42" rx="7" style="fill:var(--h-wood);stroke:var(--h-wood-dark);stroke-width:2.5"/>
<text x="117" y="174" class="h-sign">sandbox</text>
<!-- the sand, and the back of the box -->
<path d="M230 176 H770 L830 250 H170 Z" style="fill:var(--h-sand)"/>
<path d="M300 196 h3 M420 186 h3 M560 190 h3 M650 204 h3 M260 232 h3 M470 238 h3 M700 236 h3 M790 240 h3 M580 226 h3" style="stroke:var(--h-sand-dot);stroke-width:4;stroke-linecap:round"/>
<rect x="224" y="163" width="552" height="15" rx="4" style="fill:var(--h-wood-dark)"/>
<!-- market data: a screen of prices -->
<rect x="282" y="148" width="78" height="54" rx="7" style="fill:var(--h-ink)"/>
<rect x="288" y="154" width="66" height="42" rx="4" style="fill:var(--h-screen)"/>
<path d="M302 160 v30 M320 164 v28 M338 158 v26" style="stroke:var(--h-muted);stroke-width:1.5"/>
<rect x="298" y="168" width="8" height="14" rx="1.5" style="fill:#2dd4bf"/>
<rect x="316" y="172" width="8" height="14" rx="1.5" style="fill:#f87171"/>
<rect x="334" y="164" width="8" height="14" rx="1.5" style="fill:#2dd4bf"/>
<!-- a live connection: an antenna -->
<rect x="678" y="194" width="24" height="8" rx="3" style="fill:var(--h-ink)"/>
<line x1="690" y1="196" x2="690" y2="152" style="stroke:var(--h-ink);stroke-width:3.5"/>
<circle cx="690" cy="148" r="6" style="fill:var(--h-teal)"/>
<path d="M674 136 a22 22 0 0 1 32 0 M664 126 a36 36 0 0 1 52 0" style="fill:none;stroke:var(--h-teal);stroke-width:3.5;stroke-linecap:round"/>
<!-- the agent -->
<g transform="translate(500 0)">
<line x1="0" y1="86" x2="0" y2="102" style="stroke:var(--h-ink);stroke-width:3"/>
<circle cx="0" cy="80" r="7" style="fill:var(--h-amber)"/>
<path d="M-36 198 Q-62 182 -66 156" style="fill:none;stroke:var(--h-ink);stroke-width:8;stroke-linecap:round"/>
<circle cx="-66" cy="151" r="8" style="fill:var(--h-paper);stroke:var(--h-ink);stroke-width:2.5"/>
<path d="M36 202 Q60 214 70 232" style="fill:none;stroke:var(--h-ink);stroke-width:8;stroke-linecap:round"/>
<circle cx="72" cy="236" r="8" style="fill:var(--h-paper);stroke:var(--h-ink);stroke-width:2.5"/>
<rect x="-8" y="168" width="16" height="12" style="fill:var(--h-ink)"/>
<rect x="-38" y="178" width="76" height="64" rx="18" style="fill:var(--h-paper);stroke:var(--h-ink);stroke-width:3"/>
<rect x="-22" y="192" width="44" height="26" rx="6" style="fill:var(--h-indigo-soft);stroke:var(--h-ink);stroke-width:2"/>
<polyline points="-15,211 -5,203 3,207 15,197" style="fill:none;stroke:var(--h-teal);stroke-width:2.5;stroke-linecap:round;stroke-linejoin:round"/>
<rect x="-46" y="102" width="92" height="68" rx="20" style="fill:var(--h-paper);stroke:var(--h-ink);stroke-width:3"/>
<circle cx="-18" cy="134" r="7.5" style="fill:var(--h-ink)"/>
<circle cx="18" cy="134" r="7.5" style="fill:var(--h-ink)"/>
<circle cx="-15.5" cy="131.5" r="2.5" style="fill:var(--h-paper)"/>
<circle cx="20.5" cy="131.5" r="2.5" style="fill:var(--h-paper)"/>
<circle cx="-33" cy="150" r="5" style="fill:var(--h-amber);opacity:.55"/>
<circle cx="33" cy="150" r="5" style="fill:var(--h-amber);opacity:.55"/>
<path d="M-14 150 Q0 161 14 150" style="fill:none;stroke:var(--h-ink);stroke-width:3;stroke-linecap:round"/>
</g>
<ellipse cx="500" cy="244" rx="66" ry="11" style="fill:var(--h-sand)"/>
<!-- orders: buy and sell -->
<rect x="212" y="206" width="78" height="36" rx="9" style="fill:var(--h-paper);stroke:var(--h-ink);stroke-width:2.5"/>
<circle cx="233" cy="224" r="11" style="fill:#0d9488"/>
<path d="M227 228 l6 -10 l6 10 Z" style="fill:#ffffff"/>
<circle cx="269" cy="224" r="11" style="fill:#dc2626"/>
<path d="M263 220 l6 10 l6 -10 Z" style="fill:#ffffff"/>
<!-- memory: a notebook -->
<g transform="rotate(-8 370 222)">
<rect x="344" y="202" width="52" height="42" rx="4" style="fill:var(--h-amber-soft);stroke:var(--h-ink);stroke-width:2.5"/>
<path d="M354 199 v8 M364 199 v8 M374 199 v8 M384 199 v8" style="stroke:var(--h-ink);stroke-width:2.5;stroke-linecap:round"/>
<path d="M353 218 h34 M353 227 h26 M353 236 h18" style="stroke:var(--h-muted);stroke-width:2;stroke-linecap:round"/>
</g>
<!-- an AI model: a chip -->
<path d="M612 202 h-8 M612 216 h-8 M612 230 h-8 M656 202 h8 M656 216 h8 M656 230 h8 M620 194 v-8 M634 194 v-8 M648 194 v-8 M620 238 v8 M634 238 v8 M648 238 v8" style="stroke:var(--h-ink);stroke-width:3;stroke-linecap:round"/>
<rect x="612" y="194" width="44" height="44" rx="8" style="fill:var(--h-indigo)"/>
<text x="634" y="222" class="h-chip">AI</text>
<path d="M670 172 l3 7 l7 3 l-7 3 l-3 7 l-3 -7 l-7 -3 l7 -3 Z" style="fill:var(--h-amber)"/>
<!-- secrets: a sealed envelope -->
<rect x="722" y="204" width="60" height="40" rx="4" style="fill:var(--h-paper);stroke:var(--h-ink);stroke-width:2.5"/>
<path d="M724 207 L752 228 L780 207" style="fill:none;stroke:var(--h-ink);stroke-width:2.5;stroke-linejoin:round"/>
<path d="M770 230 v-6 a7 7 0 0 1 14 0 v6" style="fill:none;stroke:var(--h-ink);stroke-width:2.5"/>
<rect x="765" y="229" width="24" height="18" rx="4" style="fill:var(--h-amber);stroke:var(--h-ink);stroke-width:2"/>
<circle cx="777" cy="238" r="2.5" style="fill:var(--h-coin-ink)"/>
<!-- the sides and front of the box -->
<path d="M232 170 L172 250 M768 170 L828 250" style="stroke:var(--h-wood-dark);stroke-width:13;stroke-linecap:round"/>
<rect x="160" y="246" width="680" height="66" rx="8" style="fill:var(--h-wood)"/>
<path d="M172 279 H828" style="stroke:var(--h-wood-dark);stroke-width:2.5"/>
<rect x="156" y="240" width="26" height="76" rx="5" style="fill:var(--h-wood-dark)"/>
<rect x="818" y="240" width="26" height="76" rx="5" style="fill:var(--h-wood-dark)"/>
</svg>
</div>

It works inside a sandbox, with a fixed set of tools. An agent can:

- Place and cancel orders
- Follow its own trades, positions and balance
- Read market data, such as prices and funding rates
- Stay connected to other services
- Run an AI model its Author shipped with it
- Use secrets its Author sealed into it, which nobody else can read
- Unwind its positions neatly when a Depositor withdraws

## What it cannot do

**It cannot be swapped out.** Nobody can quietly replace an agent Depositors chose with a different one, not its Author and not us: the attested machine runs only the exact build the vault was created with, and our contracts check that. An Author who wants to change their agent publishes a new version, which gets a new vault, and every Depositor decides for themselves whether to move.

**It cannot reach past the sandbox.** It can only use the tools it's been given, and only within the limits set around it.

**It cannot control the outside world.** An agent can read anything online, but it has no say over what comes back. If a news site gets a story wrong or a price feed goes stale, the agent sees exactly that and acts on it. Coping with bad information is part of writing a good agent, and part of what a Depositor trusts when they choose one.

Next, how an AI reads every agent's code before it's built. After that, three pages on what keeps your money safe: attested machines, trade-only keys and smart contracts.

!!! tip "Writing one?"
    Agents are written in Rust against a fixed interface. [Authors](../authors.md) covers what it costs and what you earn, and [Agents](../how-it-works/agents.md) has the interface and a complete example.

[Next: AI review →](ai-review.md){ .md-button .md-button--primary }
