# AI Review

Every agent comes with a public report on its code, written by a model, locked to the [attested image](attested-machines.md), that reads all of it before the agent gets built. That includes agents whose Authors keep their code private: you can't see that code, but the Reviewer has, and you still get to read the report.

<div class="shonin-hero" markdown="0">
<svg viewBox="0 0 1000 460" role="img" aria-labelledby="reviewer-title reviewer-desc" xmlns="http://www.w3.org/2000/svg">
<title id="reviewer-title">AI Review</title>
<desc id="reviewer-desc">Inside a sealed glass booth with an attestation seal on its roof, a friendly robot wearing round glasses holds a magnifying glass over a page of source code on an easel. One line of the code glows under the lens. Out of a slot in the booth's wall, an arrow leads to a clipboard: the report card. It rates six areas, each with a coloured dot: security looks fine, financial risk has some concerns, reliability and reproducibility look fine, for outside influence the Reviewer couldn't tell, and the Author's description looks accurate.</desc>
<ellipse cx="500" cy="230" rx="490" ry="226" style="fill:var(--h-blob)"/>
<!-- the attested machine: a sealed glass booth -->
<rect x="120" y="80" width="440" height="280" rx="18" style="fill:var(--h-glass);stroke:var(--h-teal);stroke-width:3"/>
<path d="M136 112 L168 88 L178 88 L136 120 Z M136 136 L200 88 L214 88 L136 148 Z" style="fill:var(--h-shine)"/>
<rect x="108" y="62" width="464" height="24" rx="10" style="fill:var(--h-teal)"/>
<rect x="554" y="196" width="12" height="50" rx="3" style="fill:var(--h-slot)"/>
<!-- the source, on an easel -->
<path d="M196 318 L184 360 M270 318 L282 360" style="stroke:var(--h-wood);stroke-width:7;stroke-linecap:round"/>
<path d="M165 140 H282 L300 158 V320 H165 Z" style="fill:var(--h-paper);stroke:var(--h-ink);stroke-width:2.5;stroke-linejoin:round"/>
<path d="M282 140 V158 H300" style="fill:none;stroke:var(--h-ink);stroke-width:2.5;stroke-linejoin:round"/>
<text x="204" y="176" class="h-code">&lt;/&gt;</text>
<path d="M182 198 h70 M196 214 h80 M182 262 h64 M196 278 h72 M196 294 h40" style="stroke:var(--h-muted);stroke-width:4;stroke-linecap:round"/>
<rect x="190" y="222" width="88" height="16" rx="5" style="fill:var(--h-amber);opacity:.45"/>
<path d="M196 230 h66" style="stroke:var(--h-indigo);stroke-width:4;stroke-linecap:round"/>
<path d="M182 246 h52" style="stroke:var(--h-muted);stroke-width:4;stroke-linecap:round"/>
<!-- the magnifying glass -->
<path d="M292 254 L330 290" style="stroke:var(--h-ink);stroke-width:9;stroke-linecap:round"/>
<circle cx="268" cy="230" r="32" style="fill:var(--h-shine);stroke:var(--h-ink);stroke-width:5"/>
<path d="M250 216 Q256 206 268 204" style="fill:none;stroke:var(--h-paper);stroke-width:4;stroke-linecap:round;opacity:.8"/>
<!-- the Reviewer -->
<rect x="416" y="318" width="12" height="36" rx="4" style="fill:var(--h-muted)"/>
<rect x="452" y="318" width="12" height="36" rx="4" style="fill:var(--h-muted)"/>
<rect x="408" y="350" width="28" height="10" rx="4" style="fill:var(--h-ink)"/>
<rect x="444" y="350" width="28" height="10" rx="4" style="fill:var(--h-ink)"/>
<path d="M398 262 Q362 292 336 290" style="fill:none;stroke:var(--h-ink);stroke-width:7;stroke-linecap:round"/>
<circle cx="334" cy="290" r="8" style="fill:var(--h-paper);stroke:var(--h-ink);stroke-width:2.5"/>
<path d="M482 258 Q514 252 522 224" style="fill:none;stroke:var(--h-ink);stroke-width:7;stroke-linecap:round"/>
<circle cx="522" cy="219" r="8" style="fill:var(--h-paper);stroke:var(--h-ink);stroke-width:2.5"/>
<rect x="395" y="236" width="90" height="86" rx="20" style="fill:var(--h-paper);stroke:var(--h-ink);stroke-width:2.5"/>
<rect x="418" y="256" width="44" height="30" rx="6" style="fill:var(--h-amber)"/>
<text x="440" y="277" class="h-tag" style="fill:var(--h-coin-ink)">AI</text>
<rect x="432" y="226" width="16" height="12" style="fill:var(--h-ink)"/>
<line x1="440" y1="150" x2="440" y2="134" style="stroke:var(--h-ink);stroke-width:2.5"/>
<circle cx="440" cy="128" r="7" style="fill:var(--h-amber)"/>
<rect x="390" y="150" width="100" height="78" rx="22" style="fill:var(--h-paper);stroke:var(--h-ink);stroke-width:2.5"/>
<circle cx="420" cy="184" r="14" style="fill:var(--h-shine);stroke:var(--h-ink);stroke-width:3"/>
<circle cx="460" cy="184" r="14" style="fill:var(--h-shine);stroke:var(--h-ink);stroke-width:3"/>
<path d="M434 184 h12 M406 182 l-14 -4 M474 182 l14 -4" style="stroke:var(--h-ink);stroke-width:3;stroke-linecap:round"/>
<circle cx="415" cy="186" r="5" style="fill:var(--h-ink)"/>
<circle cx="455" cy="186" r="5" style="fill:var(--h-ink)"/>
<circle cx="416" cy="184" r="1.8" style="fill:var(--h-paper)"/>
<circle cx="456" cy="184" r="1.8" style="fill:var(--h-paper)"/>
<path d="M428 208 Q438 216 450 208" style="fill:none;stroke:var(--h-ink);stroke-width:2.5;stroke-linecap:round"/>
<circle cx="404" cy="206" r="5" style="fill:#f87171;opacity:.45"/>
<circle cx="476" cy="206" r="5" style="fill:#f87171;opacity:.45"/>
<!-- the seal on the roof -->
<path d="M518 86 L510 118 L520 112 L526 122 L532 90 Z M542 86 L550 118 L540 112 L534 122 L528 90 Z" style="fill:var(--h-teal)"/>
<circle cx="530" cy="74" r="24" style="fill:var(--h-amber);stroke:var(--h-amber);stroke-width:5;stroke-dasharray:3 3"/>
<path d="M519 74 l7 7 l14 -15" style="fill:none;stroke:var(--h-coin-ink);stroke-width:3.5;stroke-linecap:round;stroke-linejoin:round"/>
<!-- out through the slot, to the report card -->
<path d="M572 221 H612" style="stroke:var(--h-ink);stroke-width:4;stroke-linecap:round;stroke-dasharray:2 9"/>
<path d="M608 211 L622 221 L608 231" style="fill:none;stroke:var(--h-ink);stroke-width:4;stroke-linecap:round;stroke-linejoin:round"/>
<!-- the report card -->
<rect x="630" y="66" width="340" height="346" rx="16" style="fill:var(--h-wood)"/>
<rect x="646" y="92" width="308" height="306" rx="8" style="fill:var(--h-paper);stroke:var(--h-wood-dark);stroke-width:2"/>
<rect x="760" y="54" width="80" height="28" rx="8" style="fill:var(--h-muted);stroke:var(--h-ink);stroke-width:2.5"/>
<circle cx="800" cy="62" r="4" style="fill:var(--h-paper)"/>
<text x="800" y="128" class="h-tag">report card</text>
<path d="M674 142 h252" style="stroke:var(--h-muted);stroke-width:2"/>
<circle cx="676" cy="170" r="10" style="fill:var(--h-teal)"/>
<text x="696" y="176" class="h-tag" style="text-anchor:start">Security</text>
<circle cx="676" cy="210" r="10" style="fill:var(--h-amber)"/>
<text x="696" y="216" class="h-tag" style="text-anchor:start">Financial risk</text>
<circle cx="676" cy="250" r="10" style="fill:var(--h-teal)"/>
<text x="696" y="256" class="h-tag" style="text-anchor:start">Reliability</text>
<circle cx="676" cy="290" r="10" style="fill:var(--h-teal)"/>
<text x="696" y="296" class="h-tag" style="text-anchor:start">Reproducibility</text>
<circle cx="676" cy="330" r="9" style="fill:none;stroke:var(--h-muted);stroke-width:3"/>
<text x="696" y="336" class="h-tag" style="text-anchor:start">Outside influence</text>
<text x="936" y="176" class="h-note" style="text-anchor:end">looks fine</text>
<text x="936" y="216" class="h-note" style="text-anchor:end">some concerns</text>
<text x="936" y="256" class="h-note" style="text-anchor:end">looks fine</text>
<text x="936" y="296" class="h-note" style="text-anchor:end">looks fine</text>
<text x="936" y="336" class="h-note" style="text-anchor:end">couldn't tell</text>
<circle cx="676" cy="370" r="10" style="fill:var(--h-teal)"/>
<text x="696" y="376" class="h-tag" style="text-anchor:start">Accurate description</text>
<text x="936" y="376" class="h-note" style="text-anchor:end">looks fine</text>
<text x="340" y="444" class="h-caption">reads the source, on an attested machine</text>
<text x="800" y="444" class="h-caption">a report card, not a grade</text>
</svg>
</div>

## What it looks at

The Reviewer rates the code in six areas. It judges what the code actually does, not what its Author says it does, and then checks the two against each other:

**Security**
: Is the code trying to do harm? Attacking the machine it runs on, reaching for keys or secrets it wasn't given, trading money away on purpose, or quietly behaving differently after a certain date or signal. Text that tries to talk the Reviewer into a better report counts too, and so does leaning on little-used code libraries that few people have checked.

**Financial risk**
: Does the code itself keep its risks in check? Trades or leverage that can grow without a limit, carrying on the same way after a run of losses, patterns that have lost people money before, like doubling down after every loss, and an AI model deciding how much to trade with nothing checking its answer.

**Reliability**
: Does it cope when things go wrong? A rejected order, an exchange that's down, or a restart from scratch that leaves it confused about what it holds.

**Reproducibility**
: Would building it again give the same agent? Code that reads the clock, the network or a random number while it's being built can come out different each time, and then nobody could rebuild it to check that what's running is what they read.

**Outside influence**
: How much can something outside the code steer what it does? An agent that follows a website, a data feed or a signal can only be as good as its source, and a source its Author controls could tell it to do anything.

**Accurate description**
: Does the code do what its Author says it does? The Reviewer compares the Author's summary, and every limit they claim, like "never more than 2× leverage", with what the code actually does. A limit that's promised but never enforced counts against it.

## The report card

Each area gets one of four ratings:

- **Looks fine**: nothing worth worrying about.
- **Some concerns**: things worth reading before you decide.
- **Serious concerns**: problems a careful reader would take seriously.
- **Couldn't tell**: the Reviewer couldn't judge it. An AI model the agent ships with is a common reason: it's a file of numbers, not code, so nobody can read what it will do.

The report also has a summary of the agent in the Reviewer's own words.

## Malicious code is never built

The review is advice, with one exception. Building code can run some of it, so the Reviewer reads everything first, and if it finds code written to do harm, the agent is refused and never built. Nothing runs, and the report is public.

Anything short of that goes ahead, however it's rated. An agent with serious concerns can still open a vault. Its page shows them loudly, and whether to back it is up to each Depositor to decide for themselves.

## What it isn't

**It isn't a prediction.** The Reviewer never rates how much money an agent might make. A careful agent can lose money, and a reckless one can make it.

**It isn't a guarantee.** It's one careful reading, and any reader can miss things. What keeps your money safe is the attested machine, the [trade-only keys](trade-only-keys.md) and the [smart contracts](smart-contracts.md). The report helps you choose.

[Next: attested machines →](attested-machines.md){ .md-button .md-button--primary }
