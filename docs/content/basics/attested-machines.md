# Attested machines

An attested machine is a computer that can prove exactly what code it is running.

Inside it is a **secure enclave**: a part of the processor that measures the code it runs and signs a statement of it, called an **attestation**. Short of a flaw in the chip itself, nobody can forge that signature, not even whoever operates the machine, and that includes us.

<div class="shonin-hero" markdown="0">
<svg viewBox="0 0 1000 360" role="img" aria-labelledby="attest-title attest-desc" xmlns="http://www.w3.org/2000/svg">
<title id="attest-title">How an attested machine gets a key past the gate</title>
<desc id="attest-desc">On the left, a glass machine with a gold seal on its roof. Inside it, a chip, the secure enclave, holds a key, and a signed certificate, the attestation, slides out of a slot. In the middle, a friendly block on the chain inspects the certificate and gets a check mark. On the right, a striped barrier lifts, and the key passes through to a trading screen.</desc>
<ellipse cx="500" cy="200" rx="480" ry="150" style="fill:var(--h-blob)"/>
<ellipse cx="210" cy="312" rx="130" ry="9" style="fill:var(--h-shadow)"/>
<ellipse cx="512" cy="312" rx="80" ry="8" style="fill:var(--h-shadow)"/>
<ellipse cx="800" cy="312" rx="150" ry="9" style="fill:var(--h-shadow)"/>
<!-- the attested machine -->
<rect x="120" y="296" width="30" height="10" rx="3" style="fill:var(--h-teal)"/>
<rect x="270" y="296" width="30" height="10" rx="3" style="fill:var(--h-teal)"/>
<rect x="100" y="112" width="220" height="188" rx="20" style="fill:var(--h-glass);stroke:var(--h-teal);stroke-width:3"/>
<path d="M116 146 L146 124 L156 124 L116 154 Z M116 170 L176 124 L190 124 L116 182 Z" style="fill:var(--h-shine)"/>
<rect x="90" y="96" width="240" height="24" rx="10" style="fill:var(--h-teal)"/>
<path d="M172 176 h-10 M172 198 h-10 M172 220 h-10 M248 176 h10 M248 198 h10 M248 220 h10 M188 160 v-10 M210 160 v-10 M232 160 v-10 M188 236 v10 M210 236 v10 M232 236 v10" style="stroke:var(--h-ink);stroke-width:3;stroke-linecap:round"/>
<rect x="172" y="160" width="76" height="76" rx="12" style="fill:var(--h-indigo)"/>
<rect x="184" y="172" width="52" height="52" rx="8" style="fill:none;stroke:var(--h-paper);stroke-width:2;opacity:.45"/>
<circle cx="199" cy="198" r="8" style="fill:none;stroke:var(--h-amber);stroke-width:4.5"/>
<path d="M207 198 H233 M224 198 v8 M231 198 v6" style="stroke:var(--h-amber);stroke-width:4.5;stroke-linecap:round"/>
<path d="M306 116 L300 144 L309 139 L314 148 L319 120 Z M330 116 L336 144 L327 139 L322 148 L317 120 Z" style="fill:var(--h-teal)"/>
<circle cx="318" cy="104" r="22" style="fill:var(--h-amber);stroke:var(--h-amber);stroke-width:5;stroke-dasharray:3 3"/>
<path d="M308 104 l6.5 6.5 l13 -14" style="fill:none;stroke:var(--h-coin-ink);stroke-width:3.5;stroke-linecap:round;stroke-linejoin:round"/>
<rect x="315" y="190" width="10" height="48" rx="3" style="fill:var(--h-slot)"/>
<!-- the attestation, sliding out -->
<g transform="rotate(-6 366 214)">
<rect x="326" y="184" width="80" height="60" rx="4" style="fill:var(--h-paper);stroke:var(--h-ink);stroke-width:1.5"/>
<path d="M338 199 h40 M338 209 h52 M338 219 h30" style="stroke:var(--h-muted);stroke-width:2;stroke-linecap:round"/>
<circle cx="392" cy="231" r="9" style="fill:var(--h-amber)"/>
<path d="M388 231 l3 3 l6 -6" style="fill:none;stroke:var(--h-coin-ink);stroke-width:2;stroke-linecap:round;stroke-linejoin:round"/>
</g>
<path d="M414 192 Q452 124 486 152" style="fill:none;stroke:var(--h-muted);stroke-width:2.5;stroke-linecap:round;stroke-dasharray:1 8"/>
<!-- our contract, on the chain, checking it -->
<path d="M458 288 l6 -6 h22 l-6 6 Z" style="fill:var(--h-block-top);stroke:var(--h-indigo);stroke-width:1.2;stroke-linejoin:round"/>
<path d="M480 288 l6 -6 v16 l-6 6 Z" style="fill:var(--h-block-side);stroke:var(--h-indigo);stroke-width:1.2;stroke-linejoin:round"/>
<rect x="458" y="288" width="22" height="16" rx="1.5" style="fill:var(--h-block);stroke:var(--h-indigo);stroke-width:1.2"/>
<ellipse cx="491" cy="296" rx="4.5" ry="3" style="fill:none;stroke:var(--h-ink);stroke-width:2;opacity:.7"/>
<path d="M496 288 l6 -6 h22 l-6 6 Z" style="fill:var(--h-block-top);stroke:var(--h-indigo);stroke-width:1.2;stroke-linejoin:round"/>
<path d="M518 288 l6 -6 v16 l-6 6 Z" style="fill:var(--h-block-side);stroke:var(--h-indigo);stroke-width:1.2;stroke-linejoin:round"/>
<rect x="496" y="288" width="22" height="16" rx="1.5" style="fill:var(--h-block);stroke:var(--h-indigo);stroke-width:1.2"/>
<ellipse cx="529" cy="296" rx="4.5" ry="3" style="fill:none;stroke:var(--h-ink);stroke-width:2;opacity:.7"/>
<path d="M534 288 l6 -6 h22 l-6 6 Z" style="fill:var(--h-block-top);stroke:var(--h-indigo);stroke-width:1.2;stroke-linejoin:round"/>
<path d="M556 288 l6 -6 v16 l-6 6 Z" style="fill:var(--h-block-side);stroke:var(--h-indigo);stroke-width:1.2;stroke-linejoin:round"/>
<rect x="534" y="288" width="22" height="16" rx="1.5" style="fill:var(--h-block);stroke:var(--h-indigo);stroke-width:1.2"/>
<path d="M452 178 l22 -22 h116 l-22 22 Z" style="fill:var(--h-block-top);stroke:var(--h-indigo);stroke-width:3;stroke-linejoin:round"/>
<path d="M568 178 l22 -22 v100 l-22 22 Z" style="fill:var(--h-block-side);stroke:var(--h-indigo);stroke-width:3;stroke-linejoin:round"/>
<rect x="452" y="178" width="116" height="100" rx="6" style="fill:var(--h-block);stroke:var(--h-indigo);stroke-width:3"/>
<circle cx="492" cy="216" r="6.5" style="fill:var(--h-ink)"/>
<circle cx="528" cy="216" r="6.5" style="fill:var(--h-ink)"/>
<circle cx="494" cy="214" r="2.2" style="fill:var(--h-paper)"/>
<circle cx="530" cy="214" r="2.2" style="fill:var(--h-paper)"/>
<circle cx="478" cy="232" r="4.5" style="fill:var(--h-amber);opacity:.5"/>
<circle cx="542" cy="232" r="4.5" style="fill:var(--h-amber);opacity:.5"/>
<path d="M497 238 Q510 248 523 238" style="fill:none;stroke:var(--h-ink);stroke-width:3;stroke-linecap:round"/>
<circle cx="548" cy="130" r="17" style="fill:var(--h-teal)"/>
<path d="M540 130 l6 6 l11 -12" style="fill:none;stroke:var(--h-badge-ink);stroke-width:3.5;stroke-linecap:round;stroke-linejoin:round"/>
<!-- the gate, lifted -->
<rect x="640" y="298" width="44" height="10" rx="4" style="fill:var(--h-indigo)"/>
<rect x="654" y="198" width="16" height="104" rx="4" style="fill:var(--h-indigo)"/>
<g transform="rotate(-58 662 204)">
<rect x="662" y="196" width="160" height="16" rx="8" style="fill:var(--h-paper);stroke:var(--h-ink);stroke-width:2"/>
<rect x="686" y="196" width="14" height="16" style="fill:#dc2626"/><rect x="726" y="196" width="14" height="16" style="fill:#dc2626"/><rect x="766" y="196" width="14" height="16" style="fill:#dc2626"/><rect x="806" y="196" width="14" height="16" style="fill:#dc2626"/>
<rect x="662" y="196" width="160" height="16" rx="8" style="fill:none;stroke:var(--h-ink);stroke-width:2"/>
</g>
<circle cx="662" cy="204" r="10" style="fill:var(--h-ink)"/>
<!-- the key, going through -->
<path d="M698 248 h-14 M702 262 h-22 M698 276 h-12" style="stroke:var(--h-muted);stroke-width:2.5;stroke-linecap:round"/>
<circle cx="724" cy="262" r="13" style="fill:none;stroke:var(--h-amber);stroke-width:7"/>
<path d="M737 262 H778 M766 262 v11 M776 262 v8" style="stroke:var(--h-amber);stroke-width:7;stroke-linecap:round"/>
<!-- the market -->
<rect x="862" y="236" width="16" height="46" style="fill:var(--h-ink)"/>
<rect x="838" y="280" width="64" height="9" rx="3" style="fill:var(--h-ink)"/>
<rect x="804" y="140" width="132" height="96" rx="10" style="fill:var(--h-ink)"/>
<rect x="812" y="148" width="116" height="80" rx="5" style="fill:var(--h-screen)"/>
<path d="M834 162 v50 M858 170 v48 M882 158 v44 M906 152 v40" style="stroke:var(--h-muted);stroke-width:1.5"/>
<rect x="829" y="176" width="10" height="24" rx="2" style="fill:#2dd4bf"/>
<rect x="853" y="184" width="10" height="22" rx="2" style="fill:#f87171"/>
<rect x="877" y="170" width="10" height="24" rx="2" style="fill:#2dd4bf"/>
<rect x="901" y="162" width="10" height="20" rx="2" style="fill:#2dd4bf"/>
<text x="215" y="346" class="h-caption">proves its code</text>
<text x="512" y="346" class="h-caption">our contracts check it</text>
<text x="808" y="346" class="h-caption">then the key can trade</text>
</svg>
</div>

## Not something we invented

If this sounds like a lot to take on trust, the good news is that it isn't ours. Secure enclaves are built into ordinary processors from Intel and AMD, and they have been around for years. Cloud providers like Google, Microsoft and Amazon rent them out to anyone who needs to prove what their computers are doing, such as companies handling medical or financial data.

Ours run on Google Cloud, and the attestation our contracts check is signed by Google. So you are trusting chip makers and a cloud provider that the rest of the world already relies on, not a new invention of ours.

## What they do

Our attested machines do four jobs:

- **Review** an agent's source, before anything is built
- **Build** it, once, and sign what came out
- **Run** it, holding the vault's trading keys, which are made inside the machine and never leave it
- **Enforce** the sandbox, so every agent stays within the limits its Author set, and pays for what it uses

Each machine runs an approved program, called an **image**. An image is simply Shonin's own software around the agent: the Reviewer, the builder, the executor that runs agents, and the unwinder that finishes withdrawals (not the agents themselves). The list of approved images is public, they are open-source, and a new one only counts two days after it is added, once outside money is at stake, so anyone can check it first.

Nobody, us included, can read the secret data inside: the vault's keys, the Author's private API keys, or the agent's sealed source. The one exception is the AI model the Reviewer consults, which sees sealed source under terms that rule out training on it or keeping it.

[Next: trade-only keys →](trade-only-keys.md){ .md-button .md-button--primary }
