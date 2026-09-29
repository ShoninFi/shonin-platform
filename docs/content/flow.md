# Money flow

This page puts the last few pages together: where a vault's money sits, who pays whom, and what moves when. Then it lets you run one yourself.

<div class="shonin-hero" markdown="0">
<svg viewBox="0 0 1000 370" role="img" aria-labelledby="flow-title flow-desc" xmlns="http://www.w3.org/2000/svg">
<title id="flow-title">Where a vault's money goes</title>
<desc id="flow-desc">At the top, the vault, split in two: cash at home in the vault contract, and money trading at the venue, with arrows both ways between them. Depositors put money in and take it out on the left. From the vault, weekly fees go as shares to the Author and to Shonin. The Author fills the tank, by hand and by auto-fill, and can take money out of it. Shonin pays the running costs, gas and compute, up front, and the tank pays Shonin back with a daily bill. The venue takes its trading fees from the trading account.</desc>
<ellipse cx="500" cy="190" rx="490" ry="178" style="fill:var(--h-blob)"/>
<rect x="270" y="40" width="440" height="150" rx="18" style="fill:var(--h-paper);stroke:var(--h-indigo);stroke-width:3"/>
<text x="490" y="68" class="h-tag">the vault</text>
<rect x="290" y="84" width="180" height="86" rx="14" style="fill:var(--h-glass-disc);stroke:var(--h-teal);stroke-width:2.5"/>
<text x="380.0" y="119.0" class="h-tag">cash at home</text>
<text x="380.0" y="141.0" class="h-note">in the vault contract</text>
<rect x="510" y="84" width="180" height="86" rx="14" style="fill:var(--h-glass-disc);stroke:var(--h-teal);stroke-width:2.5"/>
<text x="600.0" y="113.0" class="h-tag">trading</text>
<text x="600.0" y="135.0" class="h-note">at the venue,</text>
<text x="600.0" y="152.0" class="h-note">which takes its fees</text>
<path d="M474 116 H504" style="fill:none;stroke:var(--h-ink);stroke-width:3;stroke-linecap:round"/>
<path d="M496 109 l8 7 l-8 7" style="fill:none;stroke:var(--h-ink);stroke-width:3;stroke-linecap:round;stroke-linejoin:round"/>
<path d="M506 140 H476" style="fill:none;stroke:var(--h-ink);stroke-width:3;stroke-linecap:round"/>
<path d="M484 133 l-8 7 l8 7" style="fill:none;stroke:var(--h-ink);stroke-width:3;stroke-linecap:round;stroke-linejoin:round"/>
<rect x="40" y="95" width="160" height="64" rx="32" style="fill:var(--h-indigo-soft);stroke:var(--h-indigo);stroke-width:2.5"/>
<text x="120.0" y="133.0" class="h-tag">Depositors</text>
<path d="M204 118 H264" style="fill:none;stroke:var(--h-ink);stroke-width:3;stroke-linecap:round"/>
<path d="M256 111 l8 7 l-8 7" style="fill:none;stroke:var(--h-ink);stroke-width:3;stroke-linecap:round;stroke-linejoin:round"/>
<path d="M266 138 H206" style="fill:none;stroke:var(--h-ink);stroke-width:3;stroke-linecap:round"/>
<path d="M214 131 l-8 7 l8 7" style="fill:none;stroke:var(--h-ink);stroke-width:3;stroke-linecap:round;stroke-linejoin:round"/>
<text x="234" y="104" class="h-note">in</text>
<text x="234" y="164" class="h-note">out</text>
<rect x="250" y="268" width="160" height="64" rx="32" style="fill:var(--h-indigo-soft);stroke:var(--h-indigo);stroke-width:2.5"/>
<text x="330.0" y="306.0" class="h-tag">Author</text>
<rect x="520" y="262" width="150" height="76" rx="14" style="fill:var(--h-amber-soft);stroke:var(--h-amber-dark);stroke-width:2.5"/>
<text x="595.0" y="306.0" class="h-tag">the tank</text>
<rect x="790" y="268" width="160" height="64" rx="32" style="fill:var(--h-indigo-soft);stroke:var(--h-indigo);stroke-width:2.5"/>
<text x="870.0" y="306.0" class="h-tag">Shonin</text>
<path d="M330 192 V262" style="fill:none;stroke:var(--h-ink);stroke-width:3;stroke-linecap:round;stroke-dasharray:3 8"/>
<path d="M323 254 l7 8 l7 -8" style="fill:none;stroke:var(--h-ink);stroke-width:3;stroke-linecap:round;stroke-linejoin:round"/>
<path d="M650 192 C700 230 800 220 850 262" style="fill:none;stroke:var(--h-ink);stroke-width:3;stroke-linecap:round;stroke-dasharray:3 8"/>
<path d="M838 257 l12 5 l-3 -13" style="fill:none;stroke:var(--h-ink);stroke-width:3;stroke-linecap:round;stroke-linejoin:round"/>
<text x="242" y="226" class="h-note">fees, as shares</text>
<text x="790" y="212" class="h-note">fees, as shares</text>
<path d="M414 288 H514" style="fill:none;stroke:var(--h-ink);stroke-width:3;stroke-linecap:round"/>
<path d="M506 281 l8 7 l-8 7" style="fill:none;stroke:var(--h-ink);stroke-width:3;stroke-linecap:round;stroke-linejoin:round"/>
<path d="M516 314 H416" style="fill:none;stroke:var(--h-ink);stroke-width:3;stroke-linecap:round"/>
<path d="M424 307 l-8 7 l8 7" style="fill:none;stroke:var(--h-ink);stroke-width:3;stroke-linecap:round;stroke-linejoin:round"/>
<text x="465" y="276" class="h-note">first fill, auto-fill</text>
<text x="465" y="340" class="h-note">take out</text>
<path d="M674 300 H784" style="fill:none;stroke:var(--h-ink);stroke-width:3;stroke-linecap:round"/>
<path d="M776 293 l8 7 l-8 7" style="fill:none;stroke:var(--h-ink);stroke-width:3;stroke-linecap:round;stroke-linejoin:round"/>
<text x="729" y="274" class="h-note">daily bill,</text>
<text x="729" y="290" class="h-note">paid back</text>
<!-- running costs, paid up front by Shonin -->
<rect x="790" y="95" width="160" height="64" rx="14" style="fill:var(--h-block);stroke:var(--h-indigo);stroke-width:2.5"/>
<text x="870" y="122" class="h-tag">running costs</text>
<text x="870" y="144" class="h-note">gas and compute</text>
<path d="M870 264 V166" style="fill:none;stroke:var(--h-ink);stroke-width:3;stroke-linecap:round"/>
<path d="M863 174 l7 -8 l7 8" style="fill:none;stroke:var(--h-ink);stroke-width:3;stroke-linecap:round;stroke-linejoin:round"/>
<text x="934" y="216" class="h-note">pays up front</text>
</svg>
</div>

A vault's money is always in one of two places: **at home**, in the vault contract, or **trading**, out at its stations and venues. Shares are a claim on both. Fees are paid by creating new shares, so they never move cash. Cash only leaves the vault when someone turns shares back into dollars, and it's paid from home first.

## Try it

This simulator runs one vault a day at a time, by the rules above. Change the numbers, step through the days, and read the log to see what the daily `keep` call does: it collects the tab from the tank, pays fees on payday, then runs auto-fill.

**Trading gain per week** stands in for how well the agent trades: how much the money it has out trading grows, or shrinks, each week.

<div class="shonin-sim" data-sim="flow" markdown="0"><noscript>The simulator needs JavaScript.</noscript></div>

A few things to try:

- **A good week.** Leave the defaults and press **Next week**. Watch the first payday: the Author earns shares, and auto-fill turns some of them into tank money, from cash at home.
- **A run on the vault.** Withdraw more than is at home, say $30,000, and see it take the slower path.
- **A tank that runs dry.** Set **Trading gain per week** to **-5** and press **Next week** a few times. With no profit, the Author earns nothing, auto-fill has nothing to use, and after about three weeks the agent stops.
- **No cash kept at home.** Set **Agent keeps at home** and **Floor** to **0**. Auto-fill then has nothing to draw on, even when the Author has earned plenty.

The simulator rounds things the real contract doesn't, such as the price a leaver sells at, and the trading account moves the same amount every day. None of it changes how the money moves.

[Next: verification →](verify.md){ .md-button .md-button--primary }
