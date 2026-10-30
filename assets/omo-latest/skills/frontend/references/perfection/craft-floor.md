# Craft floor

The measurable minimum a rendered page has to meet. The ideas follow the craft-floor guidance of [pbakaus/impeccable](https://github.com/pbakaus/impeccable) (Apache-2.0, Copyright 2025 Paul Bakaus), restated and changed for this skill; `ATTRIBUTION.md` section 6 records the license and the changes.

Load it with `README.md` for any code change. It picks no style. Every line is checked against computed values on the rendered page, never against the stylesheet, and `/visual-qa` re-checks it (A3, A13, B8, E7, F8, F9). Lines come in two strengths:

- **Floor** lines bind under every style skill and brand. `DESIGN.md` may tighten a floor but never relax it.
- **Default** lines (everything not marked floor) bind until the brief says otherwise: the brief's own words, a concrete reference, or a route the brief chose that names the value (a Layer A style skill, a Layer B brand, or the ambience route). The override covers that one item only; write it into the brief or `DESIGN.md` so `/visual-qa` treats it as a decision.

## Browser surfaces (floor)

Anything the browser paints with its own defaults is undesigned until you style it. Style each one the surface actually has, from palette tokens, in light and dark:

| Surface | Set |
|---|---|
| Selection | `::selection` background from the accent at low alpha, text in the ink colour; the system highlight never appears. |
| Caret | `caret-color` on inputs, textareas, and editable regions. |
| Scrollbars | `scrollbar-color` and `scrollbar-width: thin` on containers that keep a visible bar. Overlay scrollbars stay overlay (`/visual-qa` A5): style the bar, never reserve a gutter for it. |
| Focus ring | A `:focus-visible` outline of about 2px in the focus token with a small offset, readable on every surface colour. |
| Link underlines | An underline offset of roughly 0.15-0.25em and a thickness taken from the type scale, so the line clears descenders. |
| Figures | `font-variant-numeric: tabular-nums` wherever numbers change or line up: table cells, counters, timers, prices. |

Not required where the element does not exist on the surface, or for a native control the brief deliberately leaves unstyled.

## Type and spacing

| Check | Requirement | Measure |
|---|---|---|
| Heading steps (floor) | Display, h1, h2, h3, and the smallest heading against body each differ by 1.25x or more in size. Body-family roles (lead, body, small, caption, label) may be closer in size, but then also differ in weight, colour, or case. | computed `font-size` per role in use |
| Heading spacing | Space above a section heading is at least 1.5x the space below it, tying the heading to what follows. | computed margins, padding, and parent gap |
| Group spacing | Space between groups is at least twice the space inside a group. | computed sibling gaps and margins |
| Line length (floor) | Body text holds 65-75 characters per line on desktop and no fewer than 45 on a phone. | rendered line length at 1440 and 390 |
| Display cap | Display headings stay at 6rem or below unless the chosen route sets a larger display scale. | computed `font-size` |
| Tracking | Letter spacing stays at -0.04em or looser unless the route sets tighter display tracking; large type usually reads best around -0.02 to -0.03em, and body text stays at 0. | computed `letter-spacing` against `font-size` |

Render the real copy at every captured width; fix anything that overflows or pushes a heading onto four lines.

## Generated patterns left out of the first draft

These extend the draft guard in `SKILL.md`. Each is what a generator produces when nothing decided otherwise, so a first draft omits it; the brief or its route may still ask for one (see Default above).

- **Label above a heading.** A small uppercase or accent tag over a heading ("FEATURES", "01 / OVERVIEW"). Remove it and let the heading stand.
- **Icon tile over a heading.** A rounded square with an icon above a heading and one line of text, repeated in a grid of equal cards. Use the content's real structure instead: a list, a table, an actual image.
- **Hero metric.** One oversized number with a tiny label and a row of supporting stats. A number gets that size only if it is the decision the screen exists for.
- **Nested cards.** A bordered or shadowed box inside another. Separate inner groups with space or a neutral 1px rule.
- **Neutral gray on colour.** Secondary text on a tinted or coloured surface uses a shade of that hue or the foreground at reduced alpha, never neutral gray, which looks muddy and is the first thing to fail contrast.
- **Monospace for flavour.** Monospace only for code, data, identifiers, and measurements, never to make a heading or interface look "technical".
- **Glow instead of shadow.** A shadow has a vertical offset and a soft blur. A zero-offset coloured glow around a card or button is decoration unless the brief asked for glowing-border cards through the ambience route. Hard, blur-free offset shadows (`4px 4px 0`) only in a neobrutalist direction the brief chose.

## Where this skill departs from the source

Rules this skill already holds take precedence:

- Blur, `backdrop-filter`, and `filter: blur()` are not general motion materials; they animate only inside an ambience reveal the brief asked for (`references/design/ambience-skill.md`) and never under `prefers-reduced-motion` (`/visual-qa` G4).
- A label above a heading is a default to omit, not a ban; a style route that permits a few of them (`taste-skill.md`) is followed.
- Every surface ships light and dark (`/visual-qa` E1); themes are not chosen per product category.
- A spring the `DESIGN.md` motion tokens name is fine; bounce easing is a finding only when no token asked for it.
- A project's chosen typeface is never a finding just because it is common.
- Scrollbars follow the overlay rule above; no decorative custom scrollbars.
