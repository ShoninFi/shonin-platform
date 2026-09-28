# Shonin

**Smart contracts are public and auditable. Trading agents should be too.**

Before you trust an automated trading strategy with your money, you want to know:

- What does it do?
- What risks does its strategy take?
- Can somebody swap the code out from under my feet?
- Could anyone walk off with my money?
- Can I get out whenever I want?

**On Shonin, nobody can swap the agent you chose for different code, or make an unverified claim about what it's doing.** Every answer is verifiable, backed by a powerful combination of secure enclaves and blockchain technology.

That opens trading agents up to everyone. Depositors can put money behind strategies they can actually check, and take it back whenever they like. Authors can turn a good strategy into income, earning a share of the profit it makes on other people's capital, without ever holding their money.

<div class="shonin-hero" markdown="0">
<svg viewBox="0 0 1000 700" role="img" aria-labelledby="hero-title hero-desc" xmlns="http://www.w3.org/2000/svg">
<title id="hero-title">How Shonin fits together</title>
<desc id="hero-desc">An Author feeds source code into a sealed glass booth with an attestation seal on its roof. Inside, a small robot, the agent, trades at a screen. The booth has no door, and its only connection is a locked cable to a vault standing on a chain of blocks, where one Depositor drops in a coin while another walks away with their money. Beneath the attested machine hang the two things it does: a model reads the source and writes a report on it before anything is built, and then it is built once and signed. Beneath the vault runs the chain it is part of, and from it hang the two things the chain does: it checks the attested machine's seal, and it holds the record the returns are read from.</desc>
<ellipse cx="500" cy="236" rx="480" ry="164" style="fill:var(--h-blob)"/>
<ellipse cx="110" cy="362" rx="52" ry="8" style="fill:var(--h-shadow)"/>
<ellipse cx="390" cy="364" rx="160" ry="10" style="fill:var(--h-shadow)"/>
<ellipse cx="703" cy="364" rx="112" ry="9" style="fill:var(--h-shadow)"/>
<ellipse cx="860" cy="362" rx="40" ry="7" style="fill:var(--h-shadow)"/>
<ellipse cx="946" cy="362" rx="36" ry="7" style="fill:var(--h-shadow)"/>
<!-- the author, handing in source -->
<rect x="95" y="318" width="12" height="44" rx="5" style="fill:var(--h-hair)"/>
<rect x="113" y="318" width="12" height="44" rx="5" style="fill:var(--h-hair)"/>
<path d="M78 326 Q78 252 110 248 Q142 252 142 326 Z" style="fill:var(--h-indigo)"/>
<path d="M134 266 Q156 270 172 262" style="fill:none;stroke:var(--h-indigo);stroke-width:12;stroke-linecap:round"/>
<circle cx="176" cy="261" r="7" style="fill:var(--h-skin)"/>
<circle cx="110" cy="222" r="22" style="fill:var(--h-skin)"/>
<path d="M88 220 Q88 196 110 196 Q134 196 132 218 Q124 208 110 210 Q98 211 88 220 Z" style="fill:var(--h-hair)"/>
<g transform="rotate(-6 212 262)">
<rect x="186" y="236" width="50" height="58" rx="4" style="fill:var(--h-paper);stroke:var(--h-ink);stroke-width:1.5"/>
<text x="211" y="262" class="h-code">&lt;/&gt;</text>
<path d="M195 274 h32 M195 282 h22" style="stroke:var(--h-muted);stroke-width:2;stroke-linecap:round"/>
</g>
<!-- the attested machine: a sealed glass booth with no door -->
<rect x="250" y="120" width="280" height="240" rx="18" style="fill:var(--h-glass);stroke:var(--h-teal);stroke-width:3"/>
<path d="M266 152 L298 128 L308 128 L266 160 Z M266 176 L330 128 L344 128 L266 188 Z" style="fill:var(--h-shine)"/>
<rect x="238" y="102" width="304" height="24" rx="10" style="fill:var(--h-teal)"/>
<rect x="244" y="238" width="12" height="50" rx="3" style="fill:var(--h-slot)"/>
<!-- desk and screen -->
<rect x="340" y="302" width="8" height="58" style="fill:var(--h-wood)"/>
<rect x="494" y="302" width="8" height="58" style="fill:var(--h-wood)"/>
<rect x="330" y="290" width="182" height="12" rx="4" style="fill:var(--h-wood)"/>
<rect x="441" y="268" width="10" height="20" style="fill:var(--h-ink)"/>
<rect x="426" y="285" width="40" height="5" rx="2" style="fill:var(--h-ink)"/>
<rect x="395" y="190" width="102" height="80" rx="8" style="fill:var(--h-ink)"/>
<rect x="403" y="198" width="86" height="64" rx="4" style="fill:var(--h-screen)"/>
<path d="M415 212 v38 M431 218 v36 M447 206 v38 M463 202 v34 M479 210 v30" style="stroke:var(--h-muted);stroke-width:1.5"/>
<rect x="411" y="222" width="8" height="18" rx="1.5" style="fill:#2dd4bf"/>
<rect x="427" y="228" width="8" height="18" rx="1.5" style="fill:#f87171"/>
<rect x="443" y="214" width="8" height="20" rx="1.5" style="fill:#2dd4bf"/>
<rect x="459" y="208" width="8" height="18" rx="1.5" style="fill:#2dd4bf"/>
<rect x="475" y="216" width="8" height="16" rx="1.5" style="fill:#f87171"/>
<rect x="396" y="283" width="58" height="7" rx="2" style="fill:var(--h-muted)"/>
<!-- the agent, at work -->
<rect x="338" y="310" width="8" height="50" style="fill:var(--h-muted)"/>
<rect x="316" y="300" width="52" height="10" rx="4" style="fill:var(--h-muted)"/>
<rect x="320" y="246" width="52" height="56" rx="14" style="fill:var(--h-paper);stroke:var(--h-ink);stroke-width:2.5"/>
<rect x="331" y="260" width="30" height="22" rx="5" style="fill:var(--h-amber)"/>
<circle cx="340" cy="271" r="4" style="fill:none;stroke:var(--h-coin-ink);stroke-width:2"/>
<path d="M344 271 h11 M351 271 v4 M355 271 v3" style="stroke:var(--h-coin-ink);stroke-width:2;stroke-linecap:round"/>
<path d="M368 262 Q392 262 404 282" style="fill:none;stroke:var(--h-ink);stroke-width:7;stroke-linecap:round"/>
<rect x="340" y="234" width="12" height="14" style="fill:var(--h-ink)"/>
<line x1="346" y1="180" x2="346" y2="166" style="stroke:var(--h-ink);stroke-width:2.5"/>
<circle cx="346" cy="161" r="6" style="fill:var(--h-amber)"/>
<rect x="312" y="180" width="68" height="56" rx="16" style="fill:var(--h-paper);stroke:var(--h-ink);stroke-width:2.5"/>
<circle cx="352" cy="205" r="6" style="fill:var(--h-ink)"/>
<circle cx="369" cy="205" r="6" style="fill:var(--h-ink)"/>
<circle cx="354" cy="203" r="2" style="fill:var(--h-paper)"/>
<circle cx="371" cy="203" r="2" style="fill:var(--h-paper)"/>
<path d="M355 219 Q363 226 371 219" style="fill:none;stroke:var(--h-ink);stroke-width:2.5;stroke-linecap:round"/>
<circle cx="377" cy="216" r="4" style="fill:var(--h-amber);opacity:.55"/>
<!-- the seal on the roof -->
<path d="M510 124 L502 156 L512 150 L518 160 L524 128 Z M534 124 L542 156 L532 150 L526 160 L520 128 Z" style="fill:var(--h-teal)"/>
<circle cx="522" cy="112" r="24" style="fill:var(--h-amber);stroke:var(--h-amber);stroke-width:5;stroke-dasharray:3 3"/>
<path d="M511 112 l7 7 l14 -15" style="fill:none;stroke:var(--h-coin-ink);stroke-width:3.5;stroke-linecap:round;stroke-linejoin:round"/>
<!-- the only line out: a locked cable to the vault -->
<path d="M530 322 C566 322 568 264 600 264" style="fill:none;stroke:var(--h-ink);stroke-width:5;stroke-linecap:round"/>
<path d="M559 293 v-6 a7 7 0 0 1 14 0 v6" style="fill:none;stroke:var(--h-ink);stroke-width:3"/>
<rect x="554" y="292" width="24" height="18" rx="4" style="fill:var(--h-amber);stroke:var(--h-ink);stroke-width:2"/>
<circle cx="566" cy="301" r="2.5" style="fill:var(--h-coin-ink)"/>
<!-- the chain the vault stands on: blocks, linked -->
<path d="M606 330 l8 -8 h34 l-8 8 Z" style="fill:var(--h-block-top);stroke:var(--h-indigo);stroke-width:1.5;stroke-linejoin:round"/>
<path d="M640 330 l8 -8 v28 l-8 8 Z" style="fill:var(--h-block-side);stroke:var(--h-indigo);stroke-width:1.5;stroke-linejoin:round"/>
<rect x="606" y="330" width="34" height="28" rx="2" style="fill:var(--h-block);stroke:var(--h-indigo);stroke-width:1.5"/>
<path d="M615 340 h16 M615 347 h10" style="stroke:var(--h-indigo);stroke-width:2;stroke-linecap:round;opacity:.6"/>
<ellipse cx="650" cy="344" rx="7" ry="4.5" style="fill:none;stroke:var(--h-ink);stroke-width:2.5"/>
<path d="M654 330 l8 -8 h34 l-8 8 Z" style="fill:var(--h-block-top);stroke:var(--h-indigo);stroke-width:1.5;stroke-linejoin:round"/>
<path d="M688 330 l8 -8 v28 l-8 8 Z" style="fill:var(--h-block-side);stroke:var(--h-indigo);stroke-width:1.5;stroke-linejoin:round"/>
<rect x="654" y="330" width="34" height="28" rx="2" style="fill:var(--h-block);stroke:var(--h-indigo);stroke-width:1.5"/>
<path d="M663 340 h16 M663 347 h10" style="stroke:var(--h-indigo);stroke-width:2;stroke-linecap:round;opacity:.6"/>
<ellipse cx="698" cy="344" rx="7" ry="4.5" style="fill:none;stroke:var(--h-ink);stroke-width:2.5"/>
<path d="M702 330 l8 -8 h34 l-8 8 Z" style="fill:var(--h-block-top);stroke:var(--h-indigo);stroke-width:1.5;stroke-linejoin:round"/>
<path d="M736 330 l8 -8 v28 l-8 8 Z" style="fill:var(--h-block-side);stroke:var(--h-indigo);stroke-width:1.5;stroke-linejoin:round"/>
<rect x="702" y="330" width="34" height="28" rx="2" style="fill:var(--h-block);stroke:var(--h-indigo);stroke-width:1.5"/>
<path d="M711 340 h16 M711 347 h10" style="stroke:var(--h-indigo);stroke-width:2;stroke-linecap:round;opacity:.6"/>
<ellipse cx="746" cy="344" rx="7" ry="4.5" style="fill:none;stroke:var(--h-ink);stroke-width:2.5"/>
<path d="M750 330 l8 -8 h34 l-8 8 Z" style="fill:var(--h-block-top);stroke:var(--h-indigo);stroke-width:1.5;stroke-linejoin:round"/>
<path d="M784 330 l8 -8 v28 l-8 8 Z" style="fill:var(--h-block-side);stroke:var(--h-indigo);stroke-width:1.5;stroke-linejoin:round"/>
<rect x="750" y="330" width="34" height="28" rx="2" style="fill:var(--h-block);stroke:var(--h-indigo);stroke-width:1.5"/>
<path d="M759 340 h16 M759 347 h10" style="stroke:var(--h-indigo);stroke-width:2;stroke-linecap:round;opacity:.6"/>
<ellipse cx="598" cy="344" rx="7" ry="4.5" style="fill:none;stroke:var(--h-ink);stroke-width:2.5;opacity:.5"/>
<!-- the vault -->
<g transform="translate(0 -36)">
<rect x="600" y="168" width="190" height="190" rx="18" style="fill:var(--h-indigo-soft);stroke:var(--h-indigo);stroke-width:3"/>
<rect x="616" y="184" width="158" height="158" rx="12" style="fill:var(--h-paper);stroke:var(--h-indigo);stroke-width:2"/>
<rect x="606" y="210" width="8" height="24" rx="3" style="fill:var(--h-indigo)"/>
<rect x="606" y="292" width="8" height="24" rx="3" style="fill:var(--h-indigo)"/>
<circle cx="695" cy="263" r="38" style="fill:var(--h-indigo-soft);stroke:var(--h-indigo);stroke-width:3"/>
<path d="M695 237 v-10 M695 289 v10 M669 263 h-10 M721 263 h10" style="stroke:var(--h-indigo);stroke-width:4;stroke-linecap:round"/>
<circle cx="695" cy="263" r="11" style="fill:var(--h-indigo)"/>
<rect x="675" y="160" width="40" height="9" rx="3" style="fill:var(--h-slot)"/>
</g>
<!-- a depositor, putting a coin in -->
<path d="M806 222 Q824 36 712 114" style="fill:none;stroke:var(--h-muted);stroke-width:2.5;stroke-linecap:round;stroke-dasharray:1 8"/>
<circle cx="792" cy="103" r="10" style="fill:var(--h-amber);stroke:var(--h-coin-ink);stroke-width:1.5"/>
<text x="792" y="107.5" class="h-coin">$</text>
<rect x="846" y="322" width="11" height="40" rx="5" style="fill:var(--h-hair)"/>
<rect x="863" y="322" width="11" height="40" rx="5" style="fill:var(--h-hair)"/>
<path d="M832 328 Q832 262 860 258 Q888 262 888 328 Z" style="fill:var(--h-teal)"/>
<path d="M840 274 Q828 254 818 238" style="fill:none;stroke:var(--h-teal);stroke-width:11;stroke-linecap:round"/>
<circle cx="815" cy="232" r="7" style="fill:var(--h-skin)"/>
<circle cx="860" cy="234" r="20" style="fill:var(--h-skin)"/>
<path d="M840 232 Q841 212 860 212 Q881 212 880 230 Q872 222 860 223 Q848 224 840 232 Z" style="fill:var(--h-hair)"/>
<!-- another, walking off with their money -->
<path d="M904 290 h-14 M906 304 h-20 M904 318 h-12" style="stroke:var(--h-muted);stroke-width:2.5;stroke-linecap:round"/>
<rect x="934" y="324" width="11" height="38" rx="5" transform="rotate(10 939 324)" style="fill:var(--h-hair)"/>
<rect x="950" y="324" width="11" height="38" rx="5" transform="rotate(-12 955 324)" style="fill:var(--h-hair)"/>
<path d="M922 330 Q922 268 946 264 Q970 268 970 330 Z" style="fill:var(--h-amber)"/>
<circle cx="946" cy="242" r="18" style="fill:var(--h-skin)"/>
<path d="M928 240 Q929 222 946 222 Q965 222 964 238 Q957 231 946 232 Q935 233 928 240 Z" style="fill:var(--h-hair)"/>
<path d="M966 282 Q976 296 976 304" style="fill:none;stroke:var(--h-amber);stroke-width:10;stroke-linecap:round"/>
<path d="M968 306 q10 -6 20 0 q6 18 -10 22 q-16 -4 -10 -22 Z" style="fill:var(--h-wood);stroke:var(--h-coin-ink);stroke-width:1.5"/>
<text x="978" y="324" class="h-coin" style="fill:var(--h-paper)">$</text>
<!-- what the attested machines do: read, then build -->
<path d="M252 412 q0 10 10 10 H518 q10 0 10 -10" style="fill:none;stroke:var(--h-teal);stroke-width:3;stroke-linecap:round"/>
<path d="M300 424 V490" style="stroke:var(--h-teal);stroke-width:2.2;stroke-dasharray:6 5"/>
<path d="M480 424 V490" style="stroke:var(--h-teal);stroke-width:2.2;stroke-dasharray:6 5"/>
<!-- what the chain does: check the seal, and hold the record -->
<path d="M600 446 l6 -6 h24 l-6 6 Z" style="fill:var(--h-block-top);stroke:var(--h-indigo);stroke-width:1.2;stroke-linejoin:round"/>
<path d="M624 446 l6 -6 v18 l-6 6 Z" style="fill:var(--h-block-side);stroke:var(--h-indigo);stroke-width:1.2;stroke-linejoin:round"/>
<rect x="600" y="446" width="24" height="18" rx="1.5" style="fill:var(--h-block);stroke:var(--h-indigo);stroke-width:1.2"/>
<ellipse cx="637" cy="455" rx="5" ry="3.2" style="fill:none;stroke:var(--h-ink);stroke-width:2;opacity:.7"/>
<path d="M644 446 l6 -6 h24 l-6 6 Z" style="fill:var(--h-block-top);stroke:var(--h-indigo);stroke-width:1.2;stroke-linejoin:round"/>
<path d="M668 446 l6 -6 v18 l-6 6 Z" style="fill:var(--h-block-side);stroke:var(--h-indigo);stroke-width:1.2;stroke-linejoin:round"/>
<rect x="644" y="446" width="24" height="18" rx="1.5" style="fill:var(--h-block);stroke:var(--h-indigo);stroke-width:1.2"/>
<ellipse cx="681" cy="455" rx="5" ry="3.2" style="fill:none;stroke:var(--h-ink);stroke-width:2;opacity:.7"/>
<path d="M688 446 l6 -6 h24 l-6 6 Z" style="fill:var(--h-block-top);stroke:var(--h-indigo);stroke-width:1.2;stroke-linejoin:round"/>
<path d="M712 446 l6 -6 v18 l-6 6 Z" style="fill:var(--h-block-side);stroke:var(--h-indigo);stroke-width:1.2;stroke-linejoin:round"/>
<rect x="688" y="446" width="24" height="18" rx="1.5" style="fill:var(--h-block);stroke:var(--h-indigo);stroke-width:1.2"/>
<ellipse cx="725" cy="455" rx="5" ry="3.2" style="fill:none;stroke:var(--h-ink);stroke-width:2;opacity:.7"/>
<path d="M732 446 l6 -6 h24 l-6 6 Z" style="fill:var(--h-block-top);stroke:var(--h-indigo);stroke-width:1.2;stroke-linejoin:round"/>
<path d="M756 446 l6 -6 v18 l-6 6 Z" style="fill:var(--h-block-side);stroke:var(--h-indigo);stroke-width:1.2;stroke-linejoin:round"/>
<rect x="732" y="446" width="24" height="18" rx="1.5" style="fill:var(--h-block);stroke:var(--h-indigo);stroke-width:1.2"/>
<ellipse cx="769" cy="455" rx="5" ry="3.2" style="fill:none;stroke:var(--h-ink);stroke-width:2;opacity:.7"/>
<path d="M776 446 l6 -6 h24 l-6 6 Z" style="fill:var(--h-block-top);stroke:var(--h-indigo);stroke-width:1.2;stroke-linejoin:round"/>
<path d="M800 446 l6 -6 v18 l-6 6 Z" style="fill:var(--h-block-side);stroke:var(--h-indigo);stroke-width:1.2;stroke-linejoin:round"/>
<rect x="776" y="446" width="24" height="18" rx="1.5" style="fill:var(--h-block);stroke:var(--h-indigo);stroke-width:1.2"/>
<ellipse cx="813" cy="455" rx="5" ry="3.2" style="fill:none;stroke:var(--h-ink);stroke-width:2;opacity:.7"/>
<path d="M820 446 l6 -6 h24 l-6 6 Z" style="fill:var(--h-block-top);stroke:var(--h-indigo);stroke-width:1.2;stroke-linejoin:round"/>
<path d="M844 446 l6 -6 v18 l-6 6 Z" style="fill:var(--h-block-side);stroke:var(--h-indigo);stroke-width:1.2;stroke-linejoin:round"/>
<rect x="820" y="446" width="24" height="18" rx="1.5" style="fill:var(--h-block);stroke:var(--h-indigo);stroke-width:1.2"/>
<ellipse cx="857" cy="455" rx="5" ry="3.2" style="fill:none;stroke:var(--h-ink);stroke-width:2;opacity:.7"/>
<path d="M864 446 l6 -6 h24 l-6 6 Z" style="fill:var(--h-block-top);stroke:var(--h-indigo);stroke-width:1.2;stroke-linejoin:round"/>
<path d="M888 446 l6 -6 v18 l-6 6 Z" style="fill:var(--h-block-side);stroke:var(--h-indigo);stroke-width:1.2;stroke-linejoin:round"/>
<rect x="864" y="446" width="24" height="18" rx="1.5" style="fill:var(--h-block);stroke:var(--h-indigo);stroke-width:1.2"/>
<ellipse cx="901" cy="455" rx="5" ry="3.2" style="fill:none;stroke:var(--h-ink);stroke-width:2;opacity:.7"/>
<path d="M908 446 l6 -6 h24 l-6 6 Z" style="fill:var(--h-block-top);stroke:var(--h-indigo);stroke-width:1.2;stroke-linejoin:round"/>
<path d="M932 446 l6 -6 v18 l-6 6 Z" style="fill:var(--h-block-side);stroke:var(--h-indigo);stroke-width:1.2;stroke-linejoin:round"/>
<rect x="908" y="446" width="24" height="18" rx="1.5" style="fill:var(--h-block);stroke:var(--h-indigo);stroke-width:1.2"/>
<ellipse cx="800" cy="360" rx="3.4" ry="6" style="fill:none;stroke:var(--h-ink);stroke-width:2.2;opacity:.7"/>
<ellipse cx="800" cy="373" rx="3.4" ry="6" style="fill:none;stroke:var(--h-ink);stroke-width:2.2;opacity:.7"/>
<ellipse cx="800" cy="386" rx="3.4" ry="6" style="fill:none;stroke:var(--h-ink);stroke-width:2.2;opacity:.7"/>
<ellipse cx="800" cy="399" rx="3.4" ry="6" style="fill:none;stroke:var(--h-ink);stroke-width:2.2;opacity:.7"/>
<ellipse cx="800" cy="412" rx="3.4" ry="6" style="fill:none;stroke:var(--h-ink);stroke-width:2.2;opacity:.7"/>
<ellipse cx="800" cy="425" rx="3.4" ry="6" style="fill:none;stroke:var(--h-ink);stroke-width:2.2;opacity:.7"/>
<ellipse cx="800" cy="438" rx="3.4" ry="6" style="fill:none;stroke:var(--h-ink);stroke-width:2.2;opacity:.7"/>
<path d="M710 466 V490" style="stroke:var(--h-indigo);stroke-width:2.2;stroke-dasharray:6 5"/>
<path d="M890 466 V490" style="stroke:var(--h-indigo);stroke-width:2.2;stroke-dasharray:6 5"/>
<g transform="translate(300 572)">
<circle r="80" style="fill:var(--h-glass-disc)"/>
<rect x="-42" y="-48" width="58" height="72" rx="5" style="fill:var(--h-paper);stroke:var(--h-ink);stroke-width:1.5"/>
<text x="-13" y="-20" class="h-code">&lt;/&gt;</text>
<path d="M-33 -4 h38 M-33 6 h30 M-33 16 h22" style="stroke:var(--h-muted);stroke-width:2;stroke-linecap:round"/>
<line x1="30" y1="26" x2="46" y2="42" style="stroke:var(--h-ink);stroke-width:8;stroke-linecap:round"/>
<circle cx="15" cy="11" r="21" style="fill:var(--h-glass);stroke:var(--h-ink);stroke-width:4"/>
<path d="M4 2 q6 -8 14 -6" style="fill:none;stroke:var(--h-shine-strong);stroke-width:3;stroke-linecap:round"/>
<path d="M38 -46 l3 7 l7 3 l-7 3 l-3 7 l-3 -7 l-7 -3 l7 -3 Z M54 -24 l2 5 l5 2 l-5 2 l-2 5 l-2 -5 l-5 -2 l5 -2 Z" style="fill:var(--h-amber)"/>
<rect x="-62" y="30" width="50" height="24" rx="12" style="fill:var(--h-paper);stroke:var(--h-ink);stroke-width:2"/>
<circle cx="-49" cy="42" r="5" style="fill:var(--h-teal)"/>
<circle cx="-37" cy="42" r="5" style="fill:var(--h-amber)"/>
<circle cx="-25" cy="42" r="5" style="fill:var(--h-teal)"/>
</g>
<g transform="translate(480 572)">
<circle r="80" style="fill:var(--h-glass-disc)"/>
<circle cx="-20" cy="-18" r="21" style="fill:none;stroke:var(--h-muted);stroke-width:9;stroke-dasharray:6 5"/>
<circle cx="-20" cy="-18" r="17" style="fill:var(--h-muted)"/>
<circle cx="-20" cy="-18" r="6" style="fill:var(--h-paper)"/>
<circle cx="18" cy="-40" r="11" style="fill:none;stroke:var(--h-muted);stroke-width:7;stroke-dasharray:4 4"/>
<circle cx="18" cy="-40" r="8" style="fill:var(--h-muted)"/>
<circle cx="18" cy="-40" r="3" style="fill:var(--h-paper)"/>
<path d="M-12 6 l12 -12 h40 l-12 12 Z" style="fill:var(--h-block-top);stroke:var(--h-indigo);stroke-width:2;stroke-linejoin:round"/>
<path d="M28 6 l12 -12 v38 l-12 12 Z" style="fill:var(--h-block-side);stroke:var(--h-indigo);stroke-width:2;stroke-linejoin:round"/>
<rect x="-12" y="6" width="40" height="38" rx="2" style="fill:var(--h-block);stroke:var(--h-indigo);stroke-width:2"/>
<text x="8" y="30" class="h-tiny">wasm</text>
<circle cx="44" cy="44" r="13" style="fill:var(--h-teal)"/>
<path d="M38 44 l4 4 l8 -9" style="fill:none;stroke:var(--h-badge-ink);stroke-width:3;stroke-linecap:round;stroke-linejoin:round"/>
</g>
<g transform="translate(710 572)">
<circle r="80" style="fill:var(--h-disc)"/>
<path d="M-42 4 L-48 30 L-40 25 L-35 33 L-32 8 Z M-22 4 L-16 30 L-24 25 L-29 33 L-32 8 Z" style="fill:var(--h-teal)"/>
<circle cx="-32" cy="-6" r="20" style="fill:var(--h-amber);stroke:var(--h-amber);stroke-width:5;stroke-dasharray:3 3"/>
<path d="M-41 -6 l6 6 l12 -13" style="fill:none;stroke:var(--h-coin-ink);stroke-width:3;stroke-linecap:round;stroke-linejoin:round"/>
<path d="M14 6 l10 -10 h32 l-10 10 Z" style="fill:var(--h-block-top);stroke:var(--h-indigo);stroke-width:2;stroke-linejoin:round"/>
<path d="M46 6 l10 -10 v32 l-10 10 Z" style="fill:var(--h-block-side);stroke:var(--h-indigo);stroke-width:2;stroke-linejoin:round"/>
<rect x="14" y="6" width="32" height="32" rx="2" style="fill:var(--h-block);stroke:var(--h-indigo);stroke-width:2"/>
<path d="M22 17 h16 M22 25 h10" style="stroke:var(--h-indigo);stroke-width:2;stroke-linecap:round;opacity:.6"/>
<ellipse cx="0" cy="36" rx="7" ry="4.5" style="fill:none;stroke:var(--h-ink);stroke-width:2.5;opacity:.5"/>
<path d="M-18 -28 Q-4 -46 4 -48 M26 -12 Q20 -40 12 -48" style="fill:none;stroke:var(--h-muted);stroke-width:2.5;stroke-linecap:round;stroke-dasharray:1 7"/>
<circle cx="8" cy="-52" r="14" style="fill:var(--h-teal)"/>
<path d="M1 -52 l5 5 l9 -10" style="fill:none;stroke:var(--h-badge-ink);stroke-width:3;stroke-linecap:round;stroke-linejoin:round"/>
</g>
<g transform="translate(890 572)">
<circle r="80" style="fill:var(--h-disc)"/>
<path d="M-46 -2 H48" style="stroke:var(--h-muted);stroke-width:1.5;stroke-dasharray:3 4"/>
<polyline points="-46,-2 -28,-14 -12,-6 6,-30 24,-22 44,-48" style="fill:none;stroke:var(--h-teal);stroke-width:4;stroke-linecap:round;stroke-linejoin:round"/>
<circle cx="44" cy="-48" r="5" style="fill:var(--h-teal)"/>
<path d="M-42 20 l8 -8 h28 l-8 8 Z M2 20 l8 -8 h28 l-8 8 Z" style="fill:var(--h-block-top);stroke:var(--h-indigo);stroke-width:1.5;stroke-linejoin:round"/>
<path d="M-14 20 l8 -8 v26 l-8 8 Z M30 20 l8 -8 v26 l-8 8 Z" style="fill:var(--h-block-side);stroke:var(--h-indigo);stroke-width:1.5;stroke-linejoin:round"/>
<rect x="-42" y="20" width="28" height="26" rx="2" style="fill:var(--h-block);stroke:var(--h-indigo);stroke-width:1.5"/>
<rect x="2" y="20" width="28" height="26" rx="2" style="fill:var(--h-block);stroke:var(--h-indigo);stroke-width:1.5"/>
<ellipse cx="-6" cy="33" rx="6" ry="4" style="fill:none;stroke:var(--h-ink);stroke-width:2.5"/>
<path d="M-28 12 v-8 M16 12 v-18" style="stroke:var(--h-muted);stroke-width:2;stroke-linecap:round;stroke-dasharray:1 5"/>
</g>
<!-- captions -->
<text x="110" y="400" class="h-caption">Author</text>
<text x="390" y="400" class="h-caption">agent, in an attested machine</text>
<text x="703" y="400" class="h-caption">vault, on-chain</text>
<text x="900" y="400" class="h-caption">Depositors</text>
<text x="300" y="686" class="h-caption">reviewed by AI</text>
<text x="480" y="686" class="h-caption">built and signed</text>
<text x="710" y="686" class="h-caption">seal checked</text>
<text x="890" y="686" class="h-caption">exit anytime</text>
</svg>
</div>

[Get started: depositors →](depositors.md){ .md-button .md-button--primary }
