//! The marketing 404: any unknown path on shonin.fi. Served with a real 404
//! status (the Worker's `not_found_handling`), with a way back.

use crate::html::{Current, Footer, Page};

pub fn page() -> Page<'static> {
    Page {
        path: "/404",
        title: "Page not found · Shonin",
        description: "This page isn't on the chain, or anywhere else.",
        current: Current::Other,
        class: "page-404",
        main: MAIN,
        footer: Footer::Compact,
    }
}

const MAIN: &str = r#"<section class="nf sh-hero is-in" aria-labelledby="nf-title" data-play data-loop>
  <svg class="hexgrid" aria-hidden="true" focusable="false"><defs><pattern id="hexgrid-nf" width="56" height="97" patternUnits="userSpaceOnUse"><path d="M28 0 L56 16.2 L56 48.5 L28 64.7 L0 48.5 L0 16.2 Z M28 64.7 L28 97"/></pattern></defs><rect width="100%" height="100%" fill="url(#hexgrid-nf)"/></svg>
  <div class="wrap nf__grid">
    <div class="nf__copy">
      <p class="sh-label hero__eyebrow sh-reveal">404 · Page not found</p>
      <h1 class="nf__title sh-reveal dl-120" id="nf-title">This page isn't on the chain, or anywhere else.</h1>
      <p class="nf__body sh-reveal dl-240">The link may be old, or the agent may have a new page. Published agents never change, so a changed agent has a new address.</p>
      <div class="hero__actions sh-reveal dl-360">
        <a class="sh-btn sh-btn-hero sh-btn-lg" href="{{app}}/agents">Browse agents{{i:arrow:18}}</a>
        <a class="sh-btn sh-btn-hero-2 sh-btn-lg" href="/">Go home</a>
      </div>
      <p class="nf__status sh-reveal dl-480">Looking for a vault that was stopped? Every kill and freeze is on the <a href="{{app}}/status">status page</a>.</p>
    </div>
    <div class="nf__art">
      <div class="hero__glow sh-glow-drift" aria-hidden="true"></div>
      <svg class="art nf-art" viewBox="0 0 560 560" role="img" aria-label="An open hexagonal frame with a piece missing; a light runs around it and stops at the gap">
        <defs><radialGradient id="nf-core" cx="40%" cy="35%" r="75%"><stop class="nf-core-a" offset="0"/><stop class="nf-core-b" offset="1"/></radialGradient></defs>
        <g class="nf-orbit">
          <circle class="nf-orbit-ring" cx="280" cy="280" r="236"/>
          <circle class="nf-orbit-sky" cx="516" cy="280" r="4"/>
          <circle class="nf-orbit-dot" cx="113" cy="447" r="3"/>
        </g>
        <path class="nf-outline sh-fade-in" d="M259.4 121.9 Q280.0 110.0 300.6 121.9 L406.6 183.1 Q427.2 195.0 427.2 218.8 L427.2 341.2 Q427.2 365.0 406.6 376.9 L300.6 438.1 Q280.0 450.0 259.4 438.1 L153.4 376.9 Q132.8 365.0 132.8 341.2 L132.8 218.8 Q132.8 195.0 153.4 183.1 Z"/>
        <path class="nf-fill sh-fade-in dl-100" d="M264.5 161.0 Q280.0 152.0 295.5 161.0 L375.3 207.0 Q390.9 216.0 390.9 233.9 L390.9 326.1 Q390.9 344.0 375.3 353.0 L295.5 399.0 Q280.0 408.0 264.5 399.0 L184.7 353.0 Q169.1 344.0 169.1 326.1 L169.1 233.9 Q169.1 216.0 184.7 207.0 Z"/>
        <path class="nf-frame sh-draw dl-200" d="M355.4 195.5 L375.3 207.0 Q390.9 216.0 390.9 233.9 L390.9 326.1 Q390.9 344.0 375.3 353.0 L295.5 399.0 Q280.0 408.0 264.5 399.0 L184.7 353.0 Q169.1 344.0 169.1 326.1 L169.1 233.9 Q169.1 216.0 184.7 207.0 L264.5 161.0 Q280.0 152.0 295.5 161.0 L315.5 172.5" pathLength="1"/>
        <path class="nf-trace" d="M355.4 195.5 L375.3 207.0 Q390.9 216.0 390.9 233.9 L390.9 326.1 Q390.9 344.0 375.3 353.0 L295.5 399.0 Q280.0 408.0 264.5 399.0 L184.7 353.0 Q169.1 344.0 169.1 326.1 L169.1 233.9 Q169.1 216.0 184.7 207.0 L264.5 161.0 Q280.0 152.0 295.5 161.0 L315.5 172.5" pathLength="1"/>
        <circle class="nf-pulse" cx="315.5" cy="172.5" r="9"/>
        <g class="nf-piece"><path class="nf-piece-bar" d="M347.1 139.9 L367.9 151.9" transform="rotate(14 357.5 145.9)"/></g>
        <text class="nf-404 sh-fade-in dl-900" x="280" y="302">404</text>
        <text class="nf-note" x="398" y="128">No page at this address</text>
        <text class="nf-note" x="96" y="492">Published agents never change</text>
      </svg>
    </div>
  </div>
</section>"#;
