/* Diagrams.
 *
 * The theme ships its own Mermaid integration, and in this version it empties
 * the block without drawing anything into it. So the fences are given our own
 * class (see `custom_fences` in zensical.toml), which the theme leaves alone,
 * and this renders them: once on load, again on each instant navigation, and
 * again when the colour scheme changes, because a diagram drawn for the light
 * palette is unreadable on the dark one.
 */
(() => {
  "use strict";

  const SOURCE = "https://unpkg.com/mermaid@11/dist/mermaid.esm.min.mjs";
  const SELECTOR = "pre.shonin-diagram";

  let mermaid = null;
  let counter = 0;

  /** The palette a diagram should be drawn in, from the theme's own scheme. */
  const theme = () =>
    document.body.getAttribute("data-md-color-scheme") === "slate"
      ? "dark"
      : "default";

  /** Load Mermaid once, and configure it for the scheme in use right now. */
  const ready = async () => {
    if (!mermaid) {
      const module = await import(SOURCE);
      mermaid = module.default;
    }
    mermaid.initialize({
      startOnLoad: false,
      theme: theme(),
      securityLevel: "strict",
      flowchart: { curve: "basis", useMaxWidth: true },
      sequence: { useMaxWidth: true },
    });
    return mermaid;
  };

  /**
   * Draw every diagram on the page. The source is kept on the element so a
   * re-draw, after the reader switches the palette, still has it.
   */
  const draw = async () => {
    const blocks = [...document.querySelectorAll(SELECTOR)];
    if (blocks.length === 0) return;

    let engine;
    try {
      engine = await ready();
    } catch {
      // No network, or the CDN is down. The source stays on the page as text,
      // which is worth more than an empty box.
      return;
    }

    for (const block of blocks) {
      const code = block.querySelector("code");
      const source = block.dataset.source ?? code?.textContent ?? "";
      if (!source.trim()) continue;
      block.dataset.source = source;

      try {
        const { svg } = await engine.render(`diagram-${counter++}`, source);
        block.innerHTML = svg;
        block.classList.add("shonin-diagram--drawn");
      } catch (error) {
        // A diagram that will not parse is a mistake in the page, and saying
        // so beats a blank space that looks like a missing image.
        block.classList.add("shonin-diagram--broken");
        console.error("could not draw a diagram", error);
      }
    }
  };

  // First load, and every instant navigation the theme performs.
  if (window.document$ && typeof window.document$.subscribe === "function") {
    window.document$.subscribe(() => {
      draw();
    });
  } else if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", draw);
  } else {
    draw();
  }

  // Redraw when the reader switches between light and dark.
  new MutationObserver((records) => {
    for (const record of records) {
      if (record.attributeName === "data-md-color-scheme") {
        for (const block of document.querySelectorAll(SELECTOR)) {
          block.classList.remove("shonin-diagram--drawn");
        }
        draw();
      }
    }
  }).observe(document.body, { attributes: true });
})();
