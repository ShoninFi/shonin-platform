# Social

Open work on how Shonin looks when shared: link previews, social profiles, and anything else social. Add items as they come up; remove them when they ship.

Per-route tags are written at build time by `packages/frontend/src/bin/seo.rs`, from the copy in `packages/frontend/src/route.rs`.

## Link preview image

Link previews (X, Slack, Discord, LinkedIn, iMessage, …) currently show a title and description but no image, because we only have the SVG logo and most preview bots don't render SVG.

- [ ] Design a 1200×630 PNG share image. Keep text and logo inside the central safe area, since some platforms crop to a square.
- [ ] Add it to `packages/frontend/public/`.
- [ ] In `seo.rs`, emit `og:image` (absolute URL), `og:image:width`, `og:image:height` and `og:image:alt`, plus `twitter:image`.
- [ ] Switch `twitter:card` from `summary` to `summary_large_image`.
- [ ] Consider per-route images later (e.g. a generated card per strategy page).

## Social profiles

- [ ] Once Shonin has an X account, add `twitter:site` (the `@handle`).
- [ ] Add the official profile URLs as `sameAs` on the `Organization` in the home page's JSON-LD, so search engines tie the profiles to the site.

## Verification

After each change that affects previews, check the result on the live site:

- [ ] Facebook / Meta Sharing Debugger (also refreshes its cached preview)
- [ ] LinkedIn Post Inspector
- [ ] Paste a link into Slack or Discord and check the unfurl
