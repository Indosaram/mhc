---
name: visualize
description: Build a single self-contained HTML chart, table, diagram or small dashboard that is shown inline next to a reply, when it reads better than prose. Covers offline-only pages, host theme colours, traceable numbers, lint, and looking at the rendered page in light/dark and phone width before showing it. Load before any call that displays HTML to the user.
---

# visualize

Three ideas drive everything here:

1. **Earn the page.** Make one only when a reader gets the point faster from it than from a paragraph or a short markdown table.
2. **Every number has a trail.** The reader can see how each figure was produced.
3. **Look before you show.** Never present a page you have not rendered and inspected yourself.

Not for whole websites or apps (that is frontend work), and not for a five-row table that reads fine in markdown.

`SKILL_DIR` below means the folder that contains this file.

## Step 0 — find out how pages can be shown here

Check the session's tool list (or `tool_search`) — do not assume. Then use the first that applies:

| Case | What exists | Display | Review |
|---|---|---|---|
| A | an HTML **display** tool (e.g. `show_html_page`, `html_render`, or an mhc equivalent added later) | call it with the page and the measured height | its own preview, or case B/C for screenshots |
| B | an HTML **preview/screenshot** tool (e.g. `html_preview`) | deliver the file (case C) | use it for the four views |
| C | neither — **the normal mhc situation today** | write the file and give its path / attach it as the result | open the file through the harness `browser` skill and take the screenshots there |

Never call a display or preview tool that is not in the list. If no way to render exists at all (no tool, no browser), say plainly in the reply that the page was not visually checked.

## Procedure — each step gates the next

### 1. Compute the data
- Load the data-analysis skill if the harness has one (mhc ships `data-scientist`; optional).
- Produce every figure with a query or aggregation you can rerun, at the grain the reader needs. Keep the exact query/aggregation text: it goes on the page.
- Pick the chart that answers the question: bars for comparing categories, a line for change over time, a histogram or dot plot for distributions, a node-link diagram for dependencies, a table when exact values matter more than shape.

### 2. Design
- Load the frontend/design skill if available for hierarchy, type scale, spacing, empty and error states.
- The page rules below **override** that skill: no palette or font of your own; colours and fonts come from the host.

### 3. Build one self-contained HTML file following the page rules.
A working reference: `scripts/build-example.mjs` renders `references/example-data.csv` into `references/example-chart.html` (and `--empty` into `references/example-empty.html`).

### 4. Lint — must pass before any preview or display
- Write the page to a file if you only have it as a string.
- Run the accent-stripe check from the sibling frontend skill:
  `bun $SKILL_DIR/../frontend/scripts/perfection/check-accent-stripes.ts <page.html>` (add `--json` for machine output; `npx tsx` works if bun is missing). Non-zero exit = coloured edge stripes or accent outlines found: remove them and rerun until it exits 0. If that script is not installed, skip and note "accent-stripe lint skipped: script not available".
- Run this skill's own check: `node $SKILL_DIR/scripts/check-inline-page.mjs <page.html>`. It flags external requests, literal colours outside `var()` fallbacks, unknown or fallback-less variables, non-theme fonts, a missing "how computed" block (`data-role="method"`), and a figure (`data-role="figure"`) with no static markup. Fix and rerun until clean.

### 5. Look at it
Render four views: **light and dark** × **reply-column width and 390 px**. For each: read the screenshot, read the console, fix, repeat until clean. Measure the rendered content height (document body height) for step 6.
- Case C: open the file via the `browser` skill; set the window width and the colour scheme there (or emulate `prefers-color-scheme`).
- If Playwright and a Chromium binary are available locally, `python3 $SKILL_DIR/scripts/preview-local.py <page.html> <out_dir> [--reply-width 720] [--no-js]` does all four views with network blocked and reports height, horizontal overflow, clipped elements and console errors.
- Also check once with JavaScript disabled: the figure must still be there.

### 6. Show it
With the case A tool (pass the measured height if it takes one), or deliver the file path / attachment (cases B, C).

### 7. Write the reply
Only what the page does not already say: the takeaway in a sentence, a caveat that matters, what to do next. Don't narrate the chart.

## Page rules

- **Offline only.** Zero network requests. Inline all CSS, JS, fonts and images (`data:` URIs). No CDN libraries; if a library cannot be inlined, draw the SVG by hand.
- **Host theme for colour and type.** Use only these custom properties:
  `--background --foreground --muted --muted-foreground --card --border --primary --accent --destructive --warning --success --chart-1 --chart-2 --chart-3 --chart-4 --chart-5 --chart-6 --font-sans --font-mono --radius`
  Every `var()` carries a fallback that works in both themes, e.g. `var(--chart-1, light-dark(#2563eb, #60a5fa))` with `:root{color-scheme:light dark}`. A page opened outside the host may get none of these variables. No literal colours anywhere except inside fallbacks.
- **Blend into the reply.** Fluid width. No outer padding, border, card frame or banner around the whole page.
- **Fixed chart heights** in pixels, never aspect-ratio scaling. Never stretch an SVG unevenly if it contains text: let a `preserveAspectRatio="none"` SVG hold only shapes, and position axis and value labels in HTML by percentage beside it (see the example).
- **Static first.** The figure is plain markup/SVG that renders with scripts off; JS may only add interaction (tooltips, toggles). Some hosts show pages with scripts disabled until the user allows them.
- **Traceable.** A small, muted "how these numbers were computed" section containing the actual query or aggregation. Caveats that change the reading (rows dropped, partial period, tiny sample) sit next to the figure they affect, not in a footnote.
- **Empty and error states** designed in the same style — never a blank box.
- **Links.** Expect the host may ask the user to confirm before opening a link and show the full URL. Never put data in URLs.

## Before showing — all must be true

- [ ] The title states the finding, not the dataset name.
- [ ] Axes are labelled with units; numbers use tabular figures and sensible precision.
- [ ] Nothing clips or overflows at 390 px.
- [ ] Series are distinguishable in both themes.
- [ ] Console is free of errors.
- [ ] Accent-stripe lint passed (or skipped with the reason stated) and `check-inline-page.mjs` is clean.

## Output

- One HTML file meeting the page rules, with its path recorded when delivered as a file.
- Shown through the host tool or delivered as a file, plus a short reply that does not repeat the page.
- If a preview step could not be done, say so; never claim a check you did not run.
