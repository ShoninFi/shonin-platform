/* The money flow simulator.
 *
 * A toy model of one vault, stepped a day at a time, so a reader can watch
 * where the money sits and what the daily keep call does. It follows the rules
 * the docs describe, in round numbers: fees are worked out every day and paid
 * weekly as new shares above the high-water mark, the tab is paid from the
 * tank, auto-fill tops the tank up from the Author's shares using cash at
 * home, and withdrawals are paid from home or, past that, by closing the
 * leaver's fraction of every position. It is an illustration, not the
 * contract.
 */
(() => {
  "use strict";

  const SELECTOR = ".shonin-sim[data-sim='flow']";

  const HOLDING_FEE = 0.01; // a year
  const AUTHOR_CUT = 0.1;
  const SHONIN_CUT = 0.05;
  const MOVE_FEE = 0.001; // deposits and withdrawals, kept in the vault

  const FIELDS = [
    { key: "deposit", label: "Depositors put in at the start", unit: "$", value: 100000, step: 1000, min: 1000 },
    { key: "weekly", label: "Trading gain per week", unit: "%", value: 10, step: 1, min: -50, max: 50 },
    { key: "keepHome", label: "Agent keeps at home", unit: "%", value: 10, step: 1, min: 0, max: 100 },
    { key: "floor", label: "Floor", unit: "%", value: 5, step: 1, min: 0, max: 100 },
    { key: "cost", label: "Running costs per day", unit: "$", value: 7, step: 1, min: 0 },
    { key: "tank", label: "Tank at the start", unit: "$", value: 150, step: 10, min: 0 },
    { key: "keepAt", label: "Auto-fill: keep tank at", unit: "$", value: 150, step: 10, min: 0 },
  ];

  const money = (n) =>
    (n < 0 ? "−$" : "$") +
    Math.abs(n).toLocaleString("en-US", { minimumFractionDigits: 2, maximumFractionDigits: 2 });
  const signed = (n) => (n >= 0 ? "+" : "") + money(n);
  const price = (n) => "$" + n.toFixed(4);
  const count = (n) => n.toLocaleString("en-US", { maximumFractionDigits: 0 });

  /** Build one simulator into its element. */
  const mount = (root) => {
    if (root.dataset.mounted) return;
    root.dataset.mounted = "yes";

    root.innerHTML = `
      <div class="shonin-sim__settings">
        <span class="shonin-sim__caption">Settings</span>
        <div class="shonin-sim__inputs"></div>
      </div>
      <div class="shonin-sim__controls">
        <div class="shonin-sim__group">
          <span class="shonin-sim__caption">Time</span>
          <button type="button" data-act="day" class="shonin-sim__btn shonin-sim__btn--solid">Next day</button>
          <button type="button" data-act="week" class="shonin-sim__btn shonin-sim__btn--solid">Next week</button>
        </div>
        <div class="shonin-sim__group">
          <label class="shonin-sim__caption" for="shonin-sim-amount">A Depositor</label>
          <span class="shonin-sim__amount"><span>$</span><input id="shonin-sim-amount" type="number" data-amount value="6000" min="0" step="1000"></span>
          <button type="button" data-act="add" class="shonin-sim__btn">Deposit</button>
          <button type="button" data-act="withdraw" class="shonin-sim__btn">Withdraw</button>
        </div>
        <button type="button" data-act="reset" class="shonin-sim__reset">Start over</button>
      </div>
      <div class="shonin-sim__state"></div>
      <h4 class="shonin-sim__logtitle">What happened, newest first</h4>
      <div class="shonin-sim__log"></div>`;

    const inputs = root.querySelector(".shonin-sim__inputs");
    for (const f of FIELDS) {
      const label = document.createElement("label");
      label.innerHTML = `<span>${f.label} (${f.unit})</span>`;
      const input = document.createElement("input");
      input.type = "number";
      input.value = f.value;
      input.step = f.step;
      if (f.min !== undefined) input.min = f.min;
      if (f.max !== undefined) input.max = f.max;
      input.dataset.key = f.key;
      label.append(input);
      inputs.append(label);
    }

    const read = (key) => Number(inputs.querySelector(`[data-key='${key}']`).value) || 0;
    const amount = () => Math.max(0, Number(root.querySelector("[data-amount]").value) || 0);
    const stateEl = root.querySelector(".shonin-sim__state");
    const logEl = root.querySelector(".shonin-sim__log");

    let s;

    const value = () => s.home + s.trading;
    const shares = () => s.depositorShares + s.authorShares + s.shoninShares;

    /**
     * What the fees since the last payday come to right now. They're taken
     * out of the share value every day, and paid as shares on payday.
     */
    const owed = () => {
      const V = value();
      const S = shares();
      const days = s.day - s.lastPayday;
      const holding = V * HOLDING_FEE * days / 365;
      const gain = Math.max(0, (V - holding) / S - s.hwm) * S;
      return { holding, author: gain * AUTHOR_CUT, shonin: gain * SHONIN_CUT, gain };
    };
    const owedTotal = (o) => o.holding + o.author + o.shonin;
    /** A share's value, after the fees owed so far. */
    const perShare = () => (value() - owedTotal(owed())) / shares();

    const log = (title, lines) => {
      const entry = document.createElement("div");
      entry.className = "shonin-sim__entry";
      entry.innerHTML = `<strong>${title}</strong>` + (lines.length ? `<ul>${lines.map((l) => `<li>${l}</li>`).join("")}</ul>` : "");
      logEl.prepend(entry);
    };

    const card = (label, big, small = "", tone = "") =>
      `<div class="shonin-sim__card ${tone}"><span>${label}</span><strong>${big}</strong>${small ? `<small>${small}</small>` : ""}</div>`;

    const render = () => {
      const pps = perShare();
      const o = owed();
      const dep = s.depositorShares * pps;
      const profit = dep - s.netIn;
      const pct = s.netIn > 0 ? (profit / s.netIn) * 100 : 0;
      const payIn = 7 - ((s.day - s.lastPayday) % 7);
      stateEl.innerHTML = `
        <section>
          <h5>The vault · day ${s.day}</h5>
          <div class="shonin-sim__cards">
            ${card("Cash at home", money(s.home))}
            ${card("Trading", money(s.trading))}
            ${card("Total", money(value()))}
            ${card("One share", price(pps), `after fees owed · high-water mark ${price(s.hwm)}`)}
            ${card("Fees owed this week", money(owedTotal(o)), `paid as shares in ${payIn} day${payIn === 1 ? "" : "s"}`)}
          </div>
        </section>
        <section>
          <h5>Depositors</h5>
          <div class="shonin-sim__cards">
            ${card("Their shares", money(dep), `${count(s.depositorShares)} shares`)}
            ${card("Their profit", signed(profit), `${pct >= 0 ? "+" : ""}${pct.toFixed(2)}% on ${money(s.netIn)} put in`, profit >= 0 ? "is-up" : "is-down")}
          </div>
        </section>
        <section>
          <h5>The Author</h5>
          <div class="shonin-sim__cards">
            ${card("Tank", money(s.tank), `auto-fill keeps it at ${money(read("keepAt"))}`)}
            ${card("Their shares", money(s.authorShares * pps), `${count(s.authorShares)} shares, earned from profit`)}
            ${card("Agent", s.running ? "Running" : "Stopped", s.running ? "" : "the tank ran dry", s.running ? "is-up" : "is-down")}
          </div>
        </section>
        <section>
          <h5>Shonin</h5>
          <div class="shonin-sim__cards">
            ${card("Its shares", money(s.shoninShares * pps), `${count(s.shoninShares)} shares, from fees`)}
            ${card("Running costs collected", money(s.collected))}
          </div>
        </section>`;
    };

    /** Move cash between home and trading the way a sensible agent would. */
    const rebalance = (lines) => {
      if (!s.running) return;
      const keep = Math.max(read("keepHome"), read("floor")) / 100;
      const target = value() * keep;
      const diff = s.home - target;
      if (Math.abs(diff) < 1) return;
      if (diff > 0) {
        s.home -= diff;
        s.trading += diff;
        lines.push(`The agent sends ${money(diff)} out to trade, keeping ${money(s.home)} at home.`);
      } else {
        const back = Math.min(-diff, s.trading);
        s.home += back;
        s.trading -= back;
        lines.push(`The agent brings ${money(back)} home from trading.`);
      }
    };

    const reset = () => {
      const deposit = Math.max(read("deposit"), 1);
      s = {
        day: 0,
        lastPayday: 0,
        home: deposit,
        trading: 0,
        depositorShares: deposit, // shares start at $1.00
        authorShares: 0,
        shoninShares: 0,
        hwm: 1,
        tank: read("tank"),
        collected: 0,
        netIn: deposit,
        running: true,
      };
      logEl.innerHTML = "";
      const lines = [
        `Depositors put in ${money(deposit)} and get ${count(deposit)} shares at $1.00.`,
        `The Author puts ${money(s.tank)} in the tank.`,
      ];
      rebalance(lines);
      log("Day 0: setup", lines);
      render();
    };

    const day = () => {
      s.day += 1;
      const lines = [];
      const payday = (s.day - s.lastPayday) % 7 === 0;

      // The market, while the agent is running.
      if (s.running && s.trading > 0) {
        const r = Math.pow(1 + read("weekly") / 100, 1 / 7) - 1;
        const change = s.trading * r;
        s.trading += change;
        lines.push(`Trading ${change >= 0 ? "makes" : "loses"} ${money(Math.abs(change))}.`);
      } else if (!s.running) {
        lines.push("The agent is stopped: its positions sit where they are, unmanaged.");
      }

      // 1. The daily keep call collects the tab.
      if (s.running) {
        const cost = read("cost");
        if (s.tank >= cost) {
          s.tank -= cost;
          s.collected += cost;
          lines.push(`The daily keep call collects the ${money(cost)} tab from the tank.`);
        } else {
          s.collected += s.tank;
          lines.push(`The daily keep call collects the last ${money(s.tank)} from the tank. The tab reached what the tank held, so the agent stops.`);
          s.tank = 0;
          s.running = false;
        }
      } else {
        lines.push("The daily keep call has nothing to collect; the agent isn't running.");
      }

      // 2. Fees: worked out today, paid on payday.
      const o = owed();
      if (payday) {
        const V = value();
        const S = shares();
        const total = owedTotal(o);
        const per = S / (V - total);
        const newAuthor = o.author * per;
        const newShonin = (o.shonin + o.holding) * per;
        s.authorShares += newAuthor;
        s.shoninShares += newShonin;
        s.lastPayday = s.day;
        // Everything owed has just been paid, so a share is simply the
        // vault's value over its shares; a new high moves the mark.
        const paidUp = value() / shares();
        if (paidUp > s.hwm) s.hwm = paidUp;
        lines.push(
          o.gain > 0
            ? `Payday, at the week's first valuation. The week's fees, ${money(total)}, are paid as new shares: ${count(newAuthor)} to the Author for their ${money(o.author)}, and ${count(newShonin)} to Shonin for its ${money(o.shonin)} profit share and ${money(o.holding)} holding fee.`
            : `Payday, at the week's first valuation, but there's no new profit above the high-water mark, so no profit fee. Only the ${money(o.holding)} holding fee is paid, as ${count(newShonin)} new shares to Shonin.`,
        );
      } else {
        lines.push(`Not payday. Fees owed this week so far: ${money(owedTotal(o))}.`);
      }

      // 3. Auto-fill, from the Author's shares, using cash at home.
      const keepAt = read("keepAt");
      if (keepAt > s.tank) {
        const need = keepAt - s.tank;
        const pps = perShare();
        const amount = Math.min(need, s.authorShares * pps, s.home);
        if (amount >= 0.01) {
          s.authorShares -= amount / pps;
          s.home -= amount;
          s.tank += amount;
          lines.push(`Auto-fill cashes in ${money(amount)} of the Author's shares at the next valuation, from home, into the tank.`);
        } else if (s.authorShares * pps < 0.01) {
          lines.push("Auto-fill has nothing to use; the Author has no shares yet.");
        } else {
          lines.push("Auto-fill waits; there's no cash at home.");
        }
      } else {
        lines.push("The tank is full; auto-fill has nothing to do.");
      }

      if (!s.running && s.tank >= read("cost") && s.tank > 0) {
        s.running = true;
        lines.push("The tank has money again, so the agent starts back up.");
      }

      rebalance(lines);
      log(`Day ${s.day}${payday ? ": payday" : ""}`, lines);
      render();
    };

    const withdraw = () => {
      const pps = perShare();
      const worth = Math.min(amount(), s.depositorShares * pps);
      if (worth < 0.01) return;
      const paid = worth * (1 - MOVE_FEE);
      const lines = [];
      if (paid <= s.home) {
        s.home -= paid;
        s.depositorShares -= worth / pps;
        lines.push(`A Depositor cashes in ${money(worth)} of shares and is paid ${money(paid)} straight from home. The ${money(worth - paid)} fee stays in the vault.`);
        const floor = value() * read("floor") / 100;
        if (s.home < floor) {
          lines.push(`Home is now below the floor of ${money(floor)}. Nothing is sold, but the agent can't send more out until home is back above it.`);
        }
      } else {
        const f = worth / value();
        const fromHome = s.home * f;
        const fromTrading = s.trading * f;
        s.home -= fromHome;
        s.trading -= fromTrading;
        s.home += (fromHome + fromTrading) * MOVE_FEE;
        s.depositorShares -= worth / pps;
        lines.push(`A Depositor cashes in ${money(worth)}, more than is at home, so it takes the slower path: the vault closes their ${(f * 100).toFixed(1)}% of every position and pays them ${money(paid)} in the same transaction, from what came home and their share of the cash.`);
      }
      s.netIn -= worth;
      log(`Day ${s.day}: a withdrawal`, lines);
      render();
    };

    const add = () => {
      const put = amount();
      if (put < 0.01) return;
      const pps = perShare();
      s.home += put;
      s.depositorShares += (put * (1 - MOVE_FEE)) / pps;
      s.netIn += put;
      log(`Day ${s.day}: a deposit`, [
        `A Depositor puts in ${money(put)} and gets shares at ${price(pps)}. The ${money(put * MOVE_FEE)} fee stays in the vault.`,
      ]);
      render();
    };

    root.addEventListener("click", (event) => {
      const act = event.target.closest("[data-act]")?.dataset.act;
      if (act === "day") day();
      else if (act === "week") for (let i = 0; i < 7; i++) day();
      else if (act === "withdraw") withdraw();
      else if (act === "add") add();
      else if (act === "reset") reset();
    });

    // The starting amounts only mean something at the start; everything else
    // applies from the next day, so a reader can change course mid-run.
    inputs.addEventListener("change", (event) => {
      const key = event.target.dataset.key;
      if (key === "deposit" || key === "tank") reset();
      else {
        log(`Day ${s.day}: settings changed`, ["The new numbers apply from the next day."]);
        render();
      }
    });

    reset();
  };

  const mountAll = () => document.querySelectorAll(SELECTOR).forEach(mount);

  // First load, and every instant navigation the theme performs.
  if (window.document$ && typeof window.document$.subscribe === "function") {
    window.document$.subscribe(mountAll);
  } else if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", mountAll);
  } else {
    mountAll();
  }
})();
