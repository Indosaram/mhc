# Craft floor

> Provenance: the ideas behind this page come from Impeccable (pbakaus/impeccable), Apache-2.0, (c) 2025 Paul Bakaus. The text here is rewritten for this skill. `ATTRIBUTION.md` section 6 holds the licence and the list of what changed.

## How to use this page

Load this page alongside `references/perfection/README.md` on every task that changes code. It sets no visual direction of its own; it only fixes the minimum finish that any direction has to reach.

Prove every rule with what the browser actually computes and paints. Reading the stylesheet is not proof: cascade order, inheritance and theme switching decide the real value, so take `getComputedStyle` readings and rendered captures.

`/visual-qa` checks this page again through items A3, A13, B8, E7, F8 and F9.

### Two strengths of rule

| Class | Meaning |
|---|---|
| floor | Holds no matter which style skill (Layer A) or brand (Layer B) is loaded. `DESIGN.md` may tighten it; nothing may loosen it. |
| default | Holds until the brief overrides it. |

### What counts as a brief override

A default gives way only to one of these:

- words the brief literally states;
- a concrete visual reference the brief supplies;
- a route chosen by the brief whose own rules fix that value or pattern. Such routes are the ambience route and any Layer A style skill or Layer B brand in `references/design/`.

Examples: a brutalist route that sets a larger display size and tighter letter spacing; a brand system that puts labels above its headings; ambience-route blur reveals and glowing-border cards.

An override releases exactly the one item it names and nothing beside it. Write the deviation into the brief or `DESIGN.md` so QA reads it as a decision rather than a defect.

## Floor: surfaces the browser draws for you

If the page has one of these surfaces, it is styled from palette tokens, in the light theme and in the dark theme. A browser default left in place is a finding.

| Surface | Required styling |
|---|---|
| Text selection | `::selection` sets `color` to the ink and `background` to the accent at low alpha. The operating system's default blue never shows. |
| Insertion caret | `caret-color` (accent or ink) on `input`, `textarea` and `contenteditable`. |
| Scrollbars | Where a scroller shows a permanent bar: `scrollbar-color: <thumb> <track>` plus `scrollbar-width: thin`. Overlay scrollbars remain overlay (`/visual-qa` A5); the styling must never create a gutter. |
| Focus | `:focus-visible` draws a 2px solid outline in the focus token with `outline-offset: 2px`, and it stays visible on every surface colour it can land on. |
| Link underline | `text-underline-offset` from 0.15em to 0.25em, `text-decoration-thickness` from the type scale, and the line clears descenders. |
| Figures | `font-variant-numeric: tabular-nums` on anything that changes or has to line up: columns, counters, timers, prices, table cells. |

Exempt: a surface type the page does not contain (no inputs means no caret rule), and a native control the brief chose on purpose to leave unstyled.

## Type and spacing thresholds

| Rule | Class | Threshold | Measure with |
|---|---|---|---|
| Type scale | floor | Neighbouring heading levels (display, h1, h2, h3) differ by a `font-size` ratio of 1.25x or more; so do the last heading level and body text. Body-family roles (caption, small, body, lead, label/overline) may sit closer than 1.25x, but every neighbouring pair must also differ in weight, colour or case; size by itself does not separate them. | computed `font-size` of each role in use |
| Heading spacing | default | A heading belongs to what follows it: its top spacing is 1.5x its bottom spacing or larger. | computed margin and padding plus the parent's `gap` |
| Grouping | default | Separation between groups is 2x or more the separation among items of one group. | computed `gap` and sibling margins |
| Measure | floor | Body lines run 65-75 characters on desktop and no fewer than 45 on a phone. | rendered lines at 1440 px and 390 px |
| Display size | default | Display heading at most 6rem, unless the chosen route defines a larger display scale. | computed `font-size` |
| Tracking | default | `letter-spacing` no tighter than -0.04em, unless the route sets tighter display tracking. Large type usually reads best between -0.02em and -0.03em; body text stays at 0. | computed `letter-spacing` divided by `font-size` |

Check these with real production copy at every width you capture. Fix any overflow, and fix any heading that wraps onto 4 lines.

## Defaults: scaffold patterns kept out of the first draft

A generator falls back on these when nothing in the brief chose otherwise, so they never enter a first draft. This list extends the draft guard in `frontend/SKILL.md`. A brief or route can switch an item back on under the override rule above; the hard-offset shadow has a narrower exception, given in its row.

| Pattern | What it looks like | What to do instead |
|---|---|---|
| Kicker | A small uppercase or accent-coloured label sitting above a heading ("FEATURES", "01 - OVERVIEW"). | Delete it and let the heading stand alone. |
| Icon tile | A rounded square holding an icon, above a heading and one line of text, repeated across a grid of identical cards. | Use the structure the content really has: a list, a table, a real image. |
| Hero metric | An oversized number with a small label, flanked by side stats and an accent. | Keep it only when that number is the decision the screen exists to support. |
| Nested cards | A bordered or shadowed box inside another one. | Separate the inner groups with space or a neutral 1px rule. |
| Gray on colour | Secondary text in plain neutral gray on a tinted or coloured background; it looks muddy and is the first thing to fail contrast. | Use a darker or lighter step of the background's hue, or the ink at reduced alpha. |
| Monospace costume | A monospace face used on headings or UI to look "technical". | Reserve monospace for code, data, ids and measurements. |
| Decorative shadow | A zero-offset coloured glow on a card or button; a blur-less hard offset such as `4px 4px 0`. | Shadows carry a y-offset and a soft blur. The glow is allowed only when the brief asked for glowing-border cards via the ambience route; the hard offset only when the brief chose neobrutalism. |

## Where this skill departs from the source

Some ideas in the source conflict with the rest of this skill. Here, this skill's rules win:

- Blur (`backdrop-filter`, `filter: blur()`) is not a general animation material. Use it solely for an ambience reveal that the brief asked for (`references/design/ambience-skill.md`); when `prefers-reduced-motion` is set, drop it entirely (`/visual-qa` G4).
- A label over a heading is a default you remove, not a ban. When a route permits a bounded number of them (`taste-skill.md`), honour it.
- Both themes always ship (`/visual-qa` E1). The theme is never picked by product category.
- Spring easing is fine when `DESIGN.md` names it. Bounce easing is flagged only when no token asked for it.
- A common typeface the project chose on purpose (for example Inter to match the platform) is never a finding.
- Scrollbars follow the surface table above. Never ask for decorative custom scrollbars.
