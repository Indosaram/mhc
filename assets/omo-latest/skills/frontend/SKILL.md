---
name: frontend
description: "Builds, styles, and polishes anything drawn with HTML, CSS, or SVG: web UI, pages, components, HTML reports rendered to PDF or print, figures, PNG/SVG diagrams, slides, and images for Notion or Slack. Use for any styling, layout, animation, or visual-quality task, or when asked to make something look or feel a certain way."
---

# Frontend

This file routes a request to the rules under `references/`; the rules themselves live there. Treat the surface as a deliverable in its own right. "Clean and correct" is where the work starts, not where it ends: the target is what a top-tier product design team would put in front of customers, and a render that is tidy but visually plain does not pass. Design is never settled once and frozen; every pass over a surface is a chance to raise it. The working flow is gate -> brief -> build -> subtract -> verify, and the brief and subtraction steps exist to aim that craft, not to cap it.

Implementation work always loads the design ruleset and the perfection ruleset together, and both have to pass. A beautiful page that ships a heavy bundle fails. A perfect Lighthouse run on a page that looks generic fails too.

## Phase 0 route

Before you touch a file, write down which references this request routes to, with one reason for each, and then read exactly that set. Naming them first makes the routing something a reviewer can check. Drifting past the routed set and building from memory is how generic output gets made. A reference you never named is one you chose to skip; one you named and never opened is a debt you still owe.

**Mandatory first read for any UI work** (implementation, styling, redesign, mockup, or a visual decision): `references/design/README.md`. It holds two gates that come before everything else: no components until a design-system file exists, and the React dev tools are installed by default on a React project.

Stack each of these on top of the first read when the request touches it:

| When the request involves | Also read |
|---|---|
| Spatial structure: a new or reshaped region, shell, panel, split, list or grid, sticky bar, scroll container, "what goes where", "this breaks at X" | `references/design/layout-skill.md` for the mechanics, then `references/design/stylegallery.md` for the pattern contract and its `Layout:` line |
| Paged output: a PDF report, print stylesheet, headless-Chrome print pipeline, a deck printed to paper | `references/design/print-paged-media.md` |
| Interaction or motion: micro-interactions, transitions, gestures, hover/press/state feedback | `references/design/interaction-skill.md`; the beui.dev catalog is required reading for the nearest pattern |
| Hero atmosphere, an animated background, a typographic reveal, scroll storytelling, or card surface effects (spotlight, tilt, glow) | `references/design/ambience-skill.md`; the react-bits catalog is required, its retrofit checklist runs every time, and its code is never vendored |
| A tone neither catalog covers (AI-agent UI, charts, landing sections, brutalist or Tailwind-only builds) | `references/design/component-catalogs.md` |
| Writing, changing, or auditing frontend code | `references/perfection/README.md` (+ `react-perf-tooling.md` before a React audit): Lighthouse 100 on real Playwright Chromium, never the Lighthouse CLI, won through architecture |
| A concrete palette, font, chart, landing-structure, or UX lookup, or a design system generated from keywords | `references/ui-ux-db/README.md` and its CLI; a lookup tool, never the posture |
| Any work that creates or updates `DESIGN.md`, or an explicit ask about personas, critique, design debt, handoff, or synthetic user testing | `references/designpowers/README.md` + `lane-c-review.md`. Lane C is the final reviewer for flatness and critique, and it fills the accessibility-constraint and accepted-debt sections. Other lanes load only when their phase applies |

**Nearest-sibling shortcut (kept).** A small change (a fix, one new control, a restyled component, a copy or spacing change) on a surface with no `DESIGN.md` may copy the nearest styled sibling instead of opening the full gate: read that sibling plus at most one design reference (the Layer A skill the project follows, or `redesign-skill.md` for "this looks bad"). The shared axioms still apply. If there is no styled sibling to copy, it is not a small change: take the workflow below.

## Quick routes

Common requests and the set they load. Every route that ends in code also loads the design README and the perfection README.

| Request | Load |
|---|---|
| A landing page, no direction given | `_INDEX.md` shortlist -> one brand file + `taste-skill` (upgrade per README step 2 if the brief turns out expressive) |
| A page in a named brand's style ("like Linear", an Aside-style agent UI) | that brand's file + `taste-skill` |
| A premium, Stripe-like hero | `stripe` + `soft-skill` |
| Improving an existing dashboard | `redesign-skill` |
| Micro-interactions | add `interaction-skill` |
| A stunning hero, animated background, or headline reveal | add `ambience-skill` |
| Building from a screenshot or mock | `image-to-code-skill` + `/visual-qa` reference-fidelity mode |
| Site audit or speed work | perfection README (+ `react-perf-tooling` on React); no design files |
| A mobile mockup image, no code | `imagegen-frontend-mobile` (+ the brand if one is named) |
| A palette or font question | the ui-ux-db CLI |
| Placement is wrong or the layout breaks | `layout-skill` + `stylegallery` |
| "What do shipped apps in this space look like?" | `lazyweb` + `_INDEX.md` shortlist |
| Setting up a React project | design README + `react-dev-tooling-skill` |
| Strengthening the design workflow: personas, accessibility, debt, handoff | design README + designpowers README (+ perfection README if code follows) |

## Shared axioms (always on)

- **No `DESIGN.md`, no UI work** (the nearest-sibling shortcut is the one exception). Every colour, size, spacing, radius, and motion value traces to a token.
- **A concrete reference is the contract.** Match its pixels, copy, component structure, and responsive intent unless the user accepts a deviation.
- **Never weaken UX, and never flatten the surface, to buy a score or meet a deadline.** Swapping rendered, lit, or layered material for flat fills or bare primitives counts as weakening. Get to 100 and keep the dimension.
- **Icons come from one SVG set** (Lucide, Heroicons, Radix, Phosphor). Never an emoji.
- **No coloured edge bars and no accent outlines** (rule in Draft guard below). `check-accent-stripes.ts` must exit 0 before visual QA, and pre-existing instances on any surface you touch are removed in the same change.
- **Animate only `transform`, `opacity`, and `filter`.**
- **Motion has a cause.** Every animation and hover maps to a real interaction, state change, or affordance. Motion that only decorates is left out.
- **Done means `/visual-qa` passed**: its platform-guideline checklist, its full capture matrix (every page and sibling surface, phone and desktop, light and dark, scrolled to the end, motion driven and inspected), and its dual-reviewer completion gate on fresh evidence. This skill keeps no visual checklist of its own beyond the build rules below.

## Design-system workflow: pick one branch before any UI code

Every implementation picks exactly one branch before the first UI edit (the nearest-sibling shortcut aside).

1. **A concrete reference exists.** It is the visual contract.
   - *Static image* (screenshot, generated mock, Stitch or imagen output, Figma export, annotated packet): read `image-to-code-skill.md`. Pull the exact tokens, geometry, copy, spacing, states, and responsive intent into `DESIGN.md`, then build reusable primitives from it.
   - *Live URL or named site*: read `clone-from-url.md`. Drive a real browser and read computed styles (tokens, geometry, default/hover/focus/active states, transitions and keyframes, downloaded assets) into `DESIGN.md`, then build the primitives in code.
   - Either way the finish is `/visual-qa` reference-fidelity mode: a pixel comparison plus evidence that the result is an extensible system rather than a one-off match.
2. **Greenfield, no reference.** Research is a build step with deliverables, not optional exploration; ignore any instinct that "enough" has been explored or that a wave cap applies. Run all four lanes **in parallel** before `DESIGN.md` is written, dispatching them as concurrent subagents through the harness's `task` tool, a `team_*` team, or a `workflow` when those are available. `DESIGN.md` opens with `## 0. Research Log`, one line per lane; a lane with no line did not run. Skip a lane only when its tool or network is unavailable, and write the skip into the log.
   - *Embedded references:* shortlist 2-3 brands from `_INDEX.md`, then read one Layer A and one Layer B file **in full** (no sliced reads). Log the shortlist, the pick, and why. `open-design` only when nothing in the curated set fits; ui-ux-db for palette and type questions.
   - *Lazyweb:* read `lazyweb.md` first and run its recipe exactly as written, never ad-hoc calls. Log the queries, how many screens you actually looked at, and the layout grammar you took (never pixel copies).
   - *StyleGallery:* fetch the matching pattern and write its `Layout:` line; log the pattern and the element that owns the scroll.
   - *Imagen:* generate 2-3 drafts seeded with the loaded Layer A + B tokens, choose one, and treat it as the fidelity contract. Log the paths and the choice.

   **Commit a direction.** For an expressive brief, sketch 2-3 genuinely different directions and take the **boldest** one the loaded references can defend. Never blend them; the blend is the generic default. The reference's distinctive material survives extraction with its source named: the real elevation recipe, a multi-stop colour ramp, explicit display, body, and mono faces, and one signature interaction. Self-check: if this `DESIGN.md` could describe any generic dark SaaS, redo it.

   **Synthesise.** Sources are material: tokens, layout grammar, component anatomy, states, motion, taste decisions. Inventory the content blocks, give each block one job (hook, explain, prove, compare, convert, navigate, retain), and order sections along the visitor's decision path. Never copy logos, trademarked imagery, or brand copy. Then run the Primitive Showcase Gate.
3. **`DESIGN.md` or a component system already exists.** Read it and follow it. Update it before implementing only when the work needs something it lacks: a token, primitive, state, motion rule, accessibility constraint, accepted debt, or fidelity requirement. A layout change still gets its `Layout:` line.
4. **Existing UI, no `DESIGN.md`, no reusable components** (and not a small change). STOP and ask one focused question: keep the current look with copy-nearby styling, or extract a `DESIGN.md` and components first? Never pick silently.

**Gates, no exceptions:** the Design System Gate (`DESIGN.md` before components) and the Primitive Showcase Gate (primitives and their states pass visual QA in a showcase before any product screen). Details in `references/design/README.md`.

**`DESIGN.md` is the implementation contract.** Tokens, type, spacing, primitives, motion, responsive behaviour, accessibility constraints, and accepted debt are named there before any code uses them. Primitives, states, and screens are verified with real `/visual-qa` evidence, and for significant work the decisions, evidence, and open debt go to `/review-work`.

## 1. Brief: decide what the screen is for before drawing it

Write this for every screen you build or change, into `DESIGN.md` section 5 beside the screen's components (the `Layout:` line goes under section 4, as `references/design/stylegallery.md` says), or into the change description when the project has no `DESIGN.md`. The build follows it; `/visual-qa` checks the rendered screen against it.

- **Primary action.** Exactly one. It gets the prominent style; its peers differ by style, never by size.
- **Information priorities 1-3.** In reading order. Priority 1 is the first thing the eye lands on.
- **Left out on purpose.** What a generated version of this screen would add and this one will not: the decorative badge, the stat tile nobody acts on, the subtitle under every heading, the callout that repeats the banner.
- **Required content.** Every input, datum, label, and piece of copy the request, the reference, or the existing screen requires, listed by name. Subtraction is checked against this list; nothing on it may disappear or merge.
- **Data shape -> component** (data screens): several things to compare -> table; a ranking -> ordered list; one verdict -> conclusion banner first, evidence under it; many items -> filter + results grid; one object -> detail view with its primary action. Choose from this before reaching for cards.
- **Layout line** (any layout or structure change, an existing `DESIGN.md` included): one `Layout:` line in the format given in `references/design/stylegallery.md`, written before the code.
- **Before captures** (changing an existing UI): fix the cases now, states x widths, and capture `before` through `/visual-qa` Before/after mode BEFORE the first edit. A baseline reconstructed later is not a baseline.

## 2. Build

Build against `DESIGN.md` (or, under the nearest-sibling shortcut, the sibling) and the shared axioms. These rules are what `/visual-qa` will check afterwards; build to them so it finds nothing new.

- **Hit targets.** Touch layouts: at least 44x44 pt (44 CSS px) per interactive element, 8-12 pt between neighbours. Pointer-only: about 10 px beyond the glyph. Grow the hit area with padding or a pseudo-element, never the glyph; adjacent bar buttons share continuous hit areas.
- **Hierarchy.** The brief's primary action is the one prominent control; the most important content sits top-leading in reading order; each screen's title says what it is; neither a destructive action nor Cancel is the Enter default.
- **States.** A control ships with hover, pressed, focus-visible, and disabled states beside its default. A data view ships empty (offering the next action), loading (a placeholder at once, a spinner only past about a second), error (cause beside the object), and offline.
- **Keyboard.** Every action reachable in reading order with a visible focus ring; Escape closes the topmost overlay; Enter triggers the non-destructive default; standard shortcuts keep their meaning; icon-only controls carry an accessible name and a tooltip.
- **Motion.** Per the axioms: caused, compositor-only, interruptible, never gating input, a cross-fade under `prefers-reduced-motion`.
- **Surface.** The hero focal object and the background carry depth (see the design README's focal-object doctrine); a flat fill is a floor, not a finish.

### Draft guard: tells kept out of the first draft

Some tells regenerate by default, so they are kept out of the first draft rather than left for QA to find.

- **No coloured bar on one side of a box, rounded or square, and no accent outline on a rounded card, decorative or state.** A card, callout, warning, cover block, stat tile, testimonial, alert, or list row never carries a coloured stripe on one side: `border-left: 4px solid <accent>` or `border-l-4 border-indigo-500`, a coloured `::before`/`::after` bar inside it, an inset `box-shadow` stripe, or a hard-stop gradient band or gradient `border-image` along one edge. A rounded card never wears a 2px-or-wider accent outline either (`border: 2px solid var(--primary)`, `ring-2 ring-indigo-500`, an inset ring). Both read as generated whether they decorate the box or mark it selected, focused, or active, and an accent underline under a heading, tab, or link counts too: use a neutral 1px rule, or set the text itself in the accent ink. Give the card its identity another way: a tonal surface, an eyebrow or heading in the accent ink, a leading glyph, or more space between groups. Encode state with one ink at many alphas, a check glyph for selection, and tonal layering for focus. Still allowed: keyboard `focus-visible` rings, validation states on form inputs, neutral 1px hairlines and dividers, table rules, and a blockquote's neutral rule. This covers pre-existing instances on any surface you touch.
- **Lint before visual QA (required).** Run `bun $SKILL_DIR/scripts/perfection/check-accent-stripes.ts <changed files or dirs>` on every stylesheet, component, and HTML file you wrote, including a report or figure you will print, screenshot, or render. Exit codes: 0 clean, 1 findings, 2 usage error or unreadable target. Exit 1 means a stripe or outline is in the draft: remove it, do not argue with the finding.
- **Broader draft lint (also run).** `bun $SKILL_DIR/scripts/perfection/check-draft-guard.ts <changed files or dirs>` covers the stripes plus promotional filler copy, emoji used as icons, hype badges, and hard-coded vanity numbers in stat tiles. Treat its findings the same way.
- **No decorative badge without a state behind it.** A pill, chip, or tag appears only when it carries a value that can change: a status, a count, a category the user filters by.
- **No stat tile without a decision behind it.** A number earns a tile when someone acts on it.
- **No subtitle under every heading.** Keep a subtitle only where the heading alone would be misread.
- **One fact, one place.** A banner and a callout that restates it, a summary card and a table row with the same figure, a label repeated as its placeholder: keep one.
- **No promotional copy.** Interface copy states what a thing is or does. "Seamlessly", "effortlessly", "smart", "powerful", "at a glance", "supercharge", and their Korean counterparts ("한눈에", "손쉽게", "스마트한", "효율적으로", "간편하게") are marketing: rewrite as the plain fact or delete.

## 3. Subtract

After the draft renders and the lints exit 0, walk every element and ask what the user could not do or know without it. No answer: delete it, whatever the reference or the previous version showed. Then open the brief's required-content list and confirm every item is still on the screen by name, with its own control or field. Subtraction that merges two required inputs, drops a datum, or shortens required copy is a regression, not a cleanup. Record removals under "Left out on purpose". Subtraction removes elements without a job; it never strips depth or material from elements that keep one.

## 4. Verify

Done is the `/visual-qa` gate, not your own glance. Hand it the brief (primary action and priorities), the `Layout:` line (layout contract), the required-content list, and, for a changed surface, the `before` set so the before/after pair shows what improved. Its review loop is bounded (its own step sets the round budget); within that budget, defects **and** flatness both earn another round on every brief, not only expressive ones. A bug-free render that still looks generic is not done. Report done only when visual QA passed on fresh evidence through its dual-reviewer gate and nothing on the surface is still at minimum-effort quality; if the budget runs out first, report the remaining flatness or defects as open debt rather than as done. A concrete reference runs reference-fidelity mode. Decisions, evidence, and unresolved debt go to `/review-work` for significant work.

## The reference library (`references/`): four rulesets

### design

`references/design/` holds the architecture file, 12 Layer A taste skills, 70 Layer B brand systems, and the project-original routers. `_INDEX.md` catalogs them with mood-to-brand mappings; `README.md` carries the gates, the style routing, stacking rules, and Phase Final.

Layer 0, architecture:

| File | Read when |
|---|---|
| `design-system-architecture.md` | No `DESIGN.md` exists yet, or a system is being extracted from existing code (8 sections plus the greenfield Research Log) |

Layer A, style skills (pick AT MOST ONE; they encode opposing philosophies):

| File | Read when the brief says |
|---|---|
| `taste-skill.md` | Operational or neutral UI: internal tools, dashboards, admin, "just make it usable". Do not settle here when the brief signals glossy, premium, or startup-grade craft: those go to `soft-skill` or `gpt-tasteskill`. |
| `minimalist-skill.md` | "minimal", "clean", "Notion-like", "Linear-like", "editorial" |
| `soft-skill.md` | "premium", "luxury", "calm", "elegant", glossy or glass surfaces; always pair a high-craft Layer B (`supabase`, `linear.app`, `vercel`, `stripe`) |
| `gpt-tasteskill.md` | "Awwwards-tier", "wow factor", "cinematic", "scroll-triggered"; stacks `ambience-skill.md` |
| `brutalist-skill.md` | "brutalist", "raw", "Swiss", "experimental", "anti-design" |
| `redesign-skill.md` | Improving EXISTING UI; audit first; never on greenfield |
| `image-to-code-skill.md` | A supplied or generated image is the contract |

Layer A, stacking rows (stack on any style skill):

| File | Adds |
|---|---|
| `output-skill.md` | Output completeness: no placeholders, no TODO stubs |
| `stitch-skill.md` (worked export: `stitch-design-example.md`) | Google Stitch compatibility or a `DESIGN.md` export |
| `interaction-skill.md` | Interaction and motion mechanics from the beui.dev catalog |
| `ambience-skill.md` | Atmosphere, reveals, and scroll scenes from the react-bits catalog |
| `layout-skill.md` | Scroll ownership and CSS-contract mechanics (pairs with `stylegallery.md`) |
| `imagegen-frontend-web.md` / `imagegen-frontend-mobile.md` / `imagegen-brandkit.md` | Image output only; never code. `image-to-code-skill.md` pairs with exactly one of them |

Layer B, brands: when the user names a brand or site, load `references/design/<brand>.md` as the token source of truth: `aside` `apple` `stripe` `linear.app` `notion` `vercel` `claude` `figma` `airbnb` `nike` `tesla` `spotify` `raycast` `revolut` and ~56 more in `_INDEX.md`. Apply the tokens to the project's own content; never copy logos or trademarked imagery. A missing brand falls back to a Layer A mood match or the `open-design` skill.

React dev tooling:

| File | Read when |
|---|---|
| `react-dev-tooling-skill.md` | A React project is missing react-grab, react-scan, or react-doctor; or for per-framework snippets and the dev-only gate |

### perfection

| File | Holds |
|---|---|
| `perfection/README.md` | The seven tenets: real-browser audits only, 100 in every category, fix at the architecture, never weaken UX for points, design-system compliance, the audit report format |
| `perfection/react-perf-tooling.md` | Playwright + `playwright-lighthouse` + `react-scan/lite` injection and per-route render budgets. A perfect score with 30+ unnecessary renders on a route is not done |

Audit CLI, production build only:

```bash
uv run $SKILL_DIR/scripts/perfection/lighthouse-audit.py https://localhost:3000
```

Always run both the mobile and desktop presets, 3-5 runs each, report the median, and diagnose from the JSON output rather than the summary.

### ui-ux-db

A lookup tool for palettes, type, charts, landing structures, and UX rules; `README.md` documents the CLI and the master-plus-overrides persistence pattern. Run from the ruleset directory so it finds `data/`:

```bash
cd $SKILL_DIR/references/ui-ux-db
python3 scripts/search.py "<query>" --design-system -p "Project"   # generate a design system
python3 scripts/search.py "<query>" --domain <domain>              # one domain
python3 scripts/search.py "<query>" --stack <stack>                # stack-specific guidance
```

Domains: `product style typography color landing chart ux react web prompt`

Stacks: `html-tailwind` (default), `react nextjs vue svelte astro swiftui react-native flutter shadcn jetpack-compose`

### designpowers

`references/designpowers/README.md` routes the pinned designpowers corpus (personas, accessibility and cognitive constraints, critique, debt, handoff, synthetic user testing, motion, role prompts). Its output is distilled into `DESIGN.md` first, then into the implementation contract. It replaces nothing: not this skill, `/visual-qa`, the harness's plan and execute commands, or `/review-work`.

## When to load something else instead

| Situation | Load |
|---|---|
| Brand or style outside the 70 in `references/design/`, or the user says "Open Design" | `open-design` skill: the local open-design library (137+ design skills, 150+ design systems) |
| Driving a browser for captures | `visual-qa` skill (and `ultimate-browsing` for signed-in or bot-scored pages) |
| Pure TypeScript or logic work with zero visual surface | `programming` skill alone; this skill adds nothing there |

## Activation

Use for any frontend, web UI, UX, visual, design, styling, layout, animation, performance, accessibility, or SEO work, and for every HTML/CSS/SVG artifact that ends up as a PDF, print page, report, figure, diagram, slide, or image: building, redesigning, auditing, or generating mockups. Not for backend, CLI, or pure-logic tasks with no visual surface.
