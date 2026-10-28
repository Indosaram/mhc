
# Frontend Design Router

Read this file first for any UI work. It routes a request to the right reference files in this directory, holds the `DESIGN.md` gate and the React tooling gate, and hands verification to `/visual-qa`. The reference files carry the design rules; this file decides which to open. The brief, build rules, draft guard, and subtraction step live in the skill's `SKILL.md`; nothing here replaces them.

## The bar

The surface is the deliverable. Correct and tidy is the starting line: the target is what a top-tier product design team would ship, and a render that is clean but visually plain does not pass, whatever the product type. Design stays open for improvement on every pass rather than being decided once and locked. The brief still sets one primary action and a clear priority order, and that hierarchy is what the craft is spent on.

### Where effort goes wrong

The most common failure is spending nearly all attention on build, lint, bundle size, and test gates, then shipping the surface at its floor. Those gates are necessary and say nothing about how the page looks. If the first render only turns up bugs, you are at the **start** of the surface work, not the end: budget the remaining effort for material, colour, depth, and the signature moment.

### Focal object and atmosphere

- **A brand or product hero's focal object reads as a three-dimensional, lit object.** Use a generated bitmap, or CSS/SVG art built with light, shadow, gradient, and depth. Flat geometric shapes standing in for a brand hero are not allowed.
- **The background has depth**: a gradient, a glow, layered light, an atmospheric band, or a real image. Never a single flat fill.
- **Pick the material for the brand.** A dark, glossy brand layers tint, blur, a rim light, sheen, and glow. A bright, playful brand uses gradient fills, soft depth shadows, and a lit focal object. Glass is one option among several; whatever you pick has dimension.
- **Take the mechanisms from `ambience-skill.md`, not from memory**: the atmosphere layer and the headline reveal come from its nearest react-bits pattern and retrofit checklist.

Implementation work loads this ruleset and `../perfection/README.md` together, and both must pass: a beautiful page with a heavy bundle fails, and a perfect score on a generic-looking page fails.

## Two layers, one from each

- **Layer A, taste skills (12 files, named `-skill.md` or `imagegen-`):** how to execute. Discipline, spacing, motion physics, anti-slop guardrails, output completeness.
- **Layer B, design systems (70 brand files):** what it looks like. Concrete colour, type, and component tokens for one brand.

`_INDEX.md` lists all 83 with mood-to-brand mappings and stacking rules; read it when routing is not obvious. Most non-trivial tasks load one from each layer.

### Open Design

Use the `open-design` skill (the local open-design library, 150+ design systems) when the request names Open Design, asks for alternatives to a vendor's design tool, asks about design-system libraries, or names a brand outside the curated set. It widens the library only; this file still owns routing, the gates, and execution quality.

## Phase 0: Design System Gate (before routing, before any UI code)

Look for `DESIGN.md`, `design-system.md`, or `design-tokens.md` at the project root, then `docs/`, then `src/`.

**It exists: read all of it and follow it.** Every colour, font size, spacing value, and component pattern you produce references a token from it. A token you need and cannot find is added to `DESIGN.md` first, then used. Update it before implementation whenever the work needs a new token, primitive, state, motion rule, accessibility constraint, accepted debt, or fidelity requirement. Never introduce raw hex codes, arbitrary px values, or an ad-hoc component that bypasses the system. A layout or structure change is not covered by the existing file: it gets a `Layout:` line (`SKILL.md` Brief step) before the code.

**It does not exist: read `design-system-architecture.md` for the structure, then take the branch:**

1. **Concrete reference supplied.** `image-to-code-skill.md` for a static image (screenshot, mockup, Figma export, Stitch or imagen output, annotated packet), `clone-from-url.md` for a live site or named site: extract its exact tokens, layout geometry, copy, spacing, states (default, hover, focus, active), transitions, and responsive intent into `DESIGN.md`, then implement reusable primitives against that contract. `/visual-qa` reference-fidelity mode is the finish: pixel comparison plus proof that the code is an extensible design system, not a screenshot-matched one-off.
2. **Greenfield, no concrete reference.** Research is a build step with named deliverables. Override any "that's enough exploration" or wave-cap instinct here. Run all four lanes in parallel (as concurrent subagents via `task`, a `team_*` team, or a `workflow` when the harness offers them) before writing `DESIGN.md`, and open it with `## 0. Research Log`, one line per lane. A lane with no line did not run; skip a lane only when its tool or network is unavailable, and record the skip there.
   - *Embedded references:* shortlist 2-3 plausible Layer B candidates from `_INDEX.md`, then read ONE Layer A style skill and ONE Layer B reference in full, every line (they are 200-500 lines; a sliced read produces the flattened token set this gate exists to prevent). Log the shortlist, the pick, and why. `open-design` only when the curated set has no fit; `ui-ux-db` lookups for palette, type, and domain questions that come up here.
   - *Lazyweb real-product screens:* read `lazyweb.md` FIRST and run its recipe verbatim; it mints its own anonymous token, so never improvise curl calls against the host. Log the queries, how many screens you actually viewed, and the layout grammar taken, never pixel copies.
   - *StyleGallery:* read `stylegallery.md`, fetch the pattern whose primary spatial problem matches the screen, write the `Layout:` line, and log the pattern and its scroll owner.
   - *Imagen concept drafts:* generate 2-3 drafts, each seeded with the loaded Layer A + Layer B tokens (palette, type, material); pick the strongest and treat it as the reference-fidelity contract. Log the paths and the pick.

   **Commit a direction** before extracting tokens, in 1-2 sentences: the atmosphere, the signature material, the colour story, the one moment a visitor remembers. For an expressive brief, sketch 2-3 genuinely different directions and take the **boldest** one the loaded references can defend; never average them, the average is the generic default. The reference's distinctive material survives extraction, each decision naming its source: the actual elevation recipe (the layers that make a surface read as glass, not a single blur), a multi-stop perceptual colour ramp (not one hex at varied opacity), explicit display, body, and mono faces, one signature interaction. If your `DESIGN.md` could describe any generic dark SaaS, you flattened the reference; redo it.

   Synthesize the lanes into `DESIGN.md`. Sources are material, not mood labels: extract tokens, layout grammar, component anatomy, interaction states, motion, and taste decisions, then recombine them into project-specific primitives. Inventory the content blocks and give each a job (hook, explain, prove, compare, convert, navigate, retain), then order sections by the visitor's decision path, not by visual symmetry. Never freestyle past the selected references; never copy logos, trademarked assets, or brand-specific copy.
3. **Existing UI with implicit patterns or components.** Extract the colours, typography, spacing, primitives, states, and motion already in use into `DESIGN.md` before changing UI code.
4. **Existing UI with no `DESIGN.md` and no reusable component layer.** STOP and ask one focused question: preserve the current look with copy-nearby styling, or extract `DESIGN.md` plus reusable components first? Do not silently choose the cheaper path or the larger refactor. With copy-nearby styling the brief and the `Layout:` line live in the change description.

**Small-change rule (nearest sibling).** A small change (a fix, a new control, a restyled component, a copy or spacing change) on a surface with no `DESIGN.md` copies the nearest styled sibling: no extraction, no question, with the brief and any `Layout:` line in the change description. Branches 3 and 4 apply to a new screen or flow. When there is no styled sibling to copy, it is not a small change: take branch 3 or 4.

**Primitive Showcase Gate (required, no exceptions).** No product screen until `DESIGN.md` section 5 names the reusable primitives and their states, and each primitive plus required state passes mobile/tablet/desktop visual QA in a component showcase or equivalent state harness. Skipping it ships ad-hoc-styled screens and re-enters the redesign loop.

`DESIGN.md` is the implementation contract: tokens, type, spacing, primitives, motion, responsive behaviour, accessibility constraints, and accepted debt are named there before code uses them, and verified with real `/visual-qa` evidence. Decisions, evidence, and unresolved debt go to `/review-work` for significant work.

## Phase 0.5: React Dev Tooling Gate (React projects, on by default)

If `react` is in `package.json`, three dev-only tools are installed before any UI implementation, a small change included; the user opts out, not in. Grep the entry file (`app/layout.tsx`, `pages/_document.tsx`, `src/main.tsx`, `src/index.tsx`, `app/root.tsx`) for `react-grab` and `react-scan`, and `package.json` plus the skills directory for `react-doctor`.

Missing: run from the project root, then read `react-dev-tooling-skill.md` for per-framework manual snippets, the `*_DISABLE_REACT_DEVTOOLS` flag pattern, and the production-leak check.

```bash
npx grab@latest init                    # react-grab: UI element -> AI source context
npx react-doctor@latest install         # react-doctor: agent-skill install + static scan
npx react-scan@latest init              # react-scan: render highlighter
```

Present: open the entry file and confirm each tool sits behind `NODE_ENV === 'development'` or `import.meta.env.DEV`; fix the gate before proceeding. Skip only when the project is not React, the user said "no extra dev dependencies", or it is a React library with no entry file (react-doctor's static scan still applies).

## Routing: pick the style skill

Run in order and stop at the first match for the style skill; steps 5-10 add stacking files and can each fire on top of an earlier match.

1. **Named brand or site.**
   - *Triggers:* "make it look like Linear", "Stripe-style buttons", "Aside-style browser agent", a screenshot of a known brand site.
   - *Action:* find it under Layer B in `_INDEX.md`, load `<brand>.md` as the token source of truth, and add a Layer A for execution (`taste-skill.md` for operational surfaces; `soft-skill.md` or `gpt-tasteskill.md` when the brief is also expressive per step 2). A brand not in the index: fall through to step 2 plus a mood shortcut from the index.
2. **Ambition first, then mood.**
   - *Expressive triggers:* glossy, glassy, liquid glass, premium, luxe, expensive, startup-grade, brand-grade, high-variance, "make it beautiful", pretty, "wow", or a named product company the result should feel like.
   - *Expressive action:* commit to a high-craft Layer A (`soft-skill.md` or `gpt-tasteskill.md`) and ALWAYS pair a high-craft Layer B (`supabase`, `linear.app`, `vercel`, `stripe`). This overrides any quiet or utilitarian instinct. An expressive brief never falls through to `taste-skill.md`.
   - *Operational triggers:* internal tool, dashboard, admin, report, "just make it usable".
   - *Operational action:* `taste-skill.md` plus the `_INDEX.md` shortlist for exactly one Layer B. The bar in "The bar" still holds: operational is a quieter register, not a lower bar.
   - *Mood vocabulary:*

   | The brief says | Load |
   |---|---|
   | "minimal", "clean", "Notion-like", "Linear-like", "editorial", "boring is good" | `minimalist-skill.md` |
   | "brutalist", "raw", "Swiss", "experimental", "industrial", "anti-design", "unstyled" | `brutalist-skill.md` |
   | "premium", "luxury", "expensive", "calm", "elegant", "spa", "boutique", "glossy", "glassy", "liquid glass", "startup-grade", "make it beautiful", "pretty" | `soft-skill.md` + a high-craft Layer B (`supabase` / `linear.app` / `vercel` / `stripe`) |
   | "Awwwards-level", "wow factor", "high-variance", "magnetic", "scroll-triggered", "cinematic", "make it crazy" | `gpt-tasteskill.md` + `ambience-skill.md` for the backgrounds and scroll scenes its sections 5 and 7 demand |

   When the mood maps cleanly through an `_INDEX.md` shortcut, the Layer B brand it points to may be loaded as a concrete reference (branch 1 of Phase 0) rather than only as a token source.
3. **Redesign of existing UI.**
   - *Triggers:* "fix the design", "this looks bad", "improve the UI", "the spacing is off", an existing screenshot or codebase with a request for visual upgrades.
   - *Action:* load `redesign-skill.md`: audit first, then upgrade; stack a Layer B if the user wants a direction. Fix the before/after cases and capture `before` (`SKILL.md` Brief step) before the first edit. Never on greenfield; the audit is wasted there.
4. **Image-first.**
   - *Triggers:* "generate the design first then code it", "show me what it could look like", and **by default** any expressive greenfield brief that has no user-supplied reference.
   - *Action:* `image-to-code-skill.md` plus `imagegen-frontend-web.md` or `imagegen-frontend-mobile.md`; generate 2-3 drafts seeded with the loaded tokens, and the strongest becomes the reference-fidelity contract.
5. **Image only, no code.**
   - *Triggers:* "generate a mockup image", "brand kit board", "moodboard".
   - *Action:* only the imagegen file: `imagegen-frontend-web.md` (website mockup), `imagegen-frontend-mobile.md` (app screens), `imagegen-brandkit.md` (logo + colours + typography + mockups). No code-generation skill, or the agent writes components.
6. **Stitch or a `DESIGN.md` export.**
   - *Triggers:* "Google Stitch", "give me the design as a doc", "also a DESIGN.md".
   - *Action:* add `stitch-skill.md`; `stitch-design-example.md` shows a finished export.
7. **The agent has been lazy.**
   - *Triggers:* mid-conversation "stop with the TODOs", "stop the placeholders", "finish the implementation".
   - *Action:* add `output-skill.md`.
8. **Layout and app shells.**
   - *Triggers:* dashboard, settings, inbox, list-detail, split panes, fixed sidebar + scrolling body; a panel that won't scroll or overflows; a layout that breaks when content gets long, empty, or unbroken.
   - *Action:* add `layout-skill.md` and fetch the pattern through `stylegallery.md`. Check the two CSS contracts that fail silently: a scroll shell's flex/grid child needs a zero minimum block size (`min-height: 0`) or it never scrolls, and grid tracks must be clamped with the min-of-size-and-100% pattern (`minmax(min(<size>, 100%), 1fr)`) or they overflow narrow containers. Skip for a pure scroll-the-document marketing page.
9. **Interaction or motion.**
   - *Triggers:* micro-interactions, transitions, gestures, hover/press/focus feedback, loading/success/error morphs, "make it feel alive".
   - *Action:* add `interaction-skill.md`, find the nearest beui.dev pattern, and read its real source through the curl recipe. Extract the spring configuration, the layout strategy, the enter and exit order, and the reduced-motion path, then map them to `DESIGN.md` motion tokens.
10. **Ambience.**
    - *Triggers:* animated, shader, particle, or grid backgrounds; split, blur, shimmer, typewriter, count-up, scramble, or marquee text; scroll chapters; spotlight, tilt, glare, or glowing-border cards; any expressive hero (see the focal-object doctrine).
    - *Action:* add `ambience-skill.md`, find the nearest react-bits pattern, and read its real source. Extract the render loop and what drives it, the shader uniforms or text-split strategy, the sizing model, and the dependencies. Then run the retrofit checklist: a static state under reduced motion, pause when off-screen, no scroll listeners, compositor-only properties, `DESIGN.md` tokens, a lazy-loaded budget. One atmosphere per hero; cursor effects stay quarantined to `gpt-tasteskill.md` briefs.

### Quick lookup

| Request | Load |
|---|---|
| Landing page, no direction | `_INDEX.md` shortlist -> one brand + `taste-skill` (or step 2's expressive pair if the brief is expressive) |
| Named-brand page (Linear-style, Aside-style agent) | that brand + `taste-skill` |
| Premium Stripe-like hero | `stripe` + `soft-skill` |
| Notion-like minimal | `notion` + `minimalist-skill` |
| Brutalist portfolio | `brutalist-skill` (+ optionally `nike`) |
| Awwwards-tier scroll experience | `gpt-tasteskill` (+ `ambience-skill`) |
| Improve an existing dashboard | `redesign-skill` |
| Micro-interactions | add `interaction-skill` |
| Stunning hero, animated background, headline reveal | add `ambience-skill` |
| Build from a screenshot or mock | `image-to-code-skill` + reference-fidelity mode |
| Mobile mockup image only | `imagegen-frontend-mobile` (+ brand if named) |
| Brand identity board | `imagegen-brandkit` |
| "Stop the placeholders" | add `output-skill` |
| "Also give me a DESIGN.md doc" | add `stitch-skill` |
| App shell | the style skill + `layout-skill` |
| A panel won't scroll or overflows | add `layout-skill` |

### Stacking

1. At most ONE Layer A style skill at a time.
2. `taste-skill` and `gpt-tasteskill` are style skills too: alternatives at different intensities, alongside `minimalist`, `brutalist`, and `soft`, with opposing spacing and typography philosophies.
3. `output-skill.md` stacks on any style skill.
4. `stitch-skill.md` stacks on any style skill.
5. `redesign-skill.md` replaces the style skill when the task is auditing, not building.
6. `image-to-code-skill.md` pairs with exactly one imagegen file.
7. Layer B is orthogonal to Layer A: any brand with any style skill. Layer B supplies the tokens; Layer A drives the execution.
8. `layout-skill.md`, `interaction-skill.md`, and `ambience-skill.md` each stack on any style skill.
9. Those three also stack with each other and with `output`/`stitch`: they carry mechanics, never visual direction.

## Execution checklist after routing

1. `DESIGN.md` was read or created in Phase 0 (or, under the small-change rule, the styled sibling was copied); the screen brief (`SKILL.md`) is written.
2. Verify dependencies in `package.json`; never assume `framer-motion`, `gsap`, `lucide-react`, or `tailwindcss` (and which version). If missing, output the install command first.
3. Tailwind version lock: v4 uses `@tailwindcss/postcss` or the Vite plugin, never `tailwindcss` in `postcss.config.js`; v3 uses different config syntax. Pick from what is installed.
4. Full-height heroes use `min-h-[100dvh]`, never `h-screen` (iOS Safari jumps).
5. Next.js: motion, state, and portals live in a `'use client'` leaf, never at the page level.
6. Match the project's existing patterns FIRST: CSS Modules stay CSS Modules, styled-components stay styled-components. The references guide style, not infrastructure.
7. Every token traces to `DESIGN.md`; a pattern used 2+ times is documented back into section 5.
8. No emoji anywhere: code, markup, alt text, or visible UI. Icons come from one SVG set.
9. The hero focal object and background follow the focal-object doctrine above: dimensional, lit, never one flat fill.
10. No generic-default drift (expressive briefs): the declared fonts actually load, with no silent fallback to Inter or system fonts; elevated surfaces use the declared multi-layer recipe, not a lone `backdrop-filter: blur`; colour comes from ramp stops, not one tint at varied opacity. Grep the shipped styles for all three before QA.
11. `check-accent-stripes.ts` exits 0 (required) and `check-draft-guard.ts` is clean (the broader `SKILL.md` draft guard) before Phase Final.

## Anti-patterns

- Loading nothing and freestyling: purple-blue gradients, rounded-2xl everywhere, three feature cards, Inter, lorem ipsum. The references exist to prevent this.
- Ignoring the named brand: "Linear-style" that does not match Linear's purple, tight spacing, and mono accents is a routing failure.
- Applying a Layer B verbatim to a project that is not that brand: extract the tokens, apply them to the project's content, never copy logos or trademarked imagery.
- Using an imagegen file to write code: they are image-only; switch to `image-to-code-skill.md`.
- `as any` or `@ts-ignore` to make a borrowed component fit: adapt it cleanly.
- Flattening a lit or layered surface into a single fill to save bytes or time.
- **Coloured edge bars and accent outlines.** A box (card, callout, alert, stat tile, list row, testimonial), rounded or square, never gets a coloured stripe along one side. That includes a thick coloured left border, a Tailwind left-border utility in an accent colour, a coloured pseudo-element bar pinned to one edge, an inset shadow used as a stripe, and a hard-stop gradient or gradient border-image running down one side. A rounded card also never wears an accent-coloured outline 2px or thicker, whether a border or a ring, and an accent underline under a heading, tab, or link is the same tell. It does not matter whether the mark is decoration or a selected/active/focused state. Use a tonal surface, accent-coloured heading or eyebrow text, a leading glyph, or spacing for identity, and alpha washes of one ink or a check glyph for state. Still allowed: keyboard `focus-visible` rings, validation states on inputs, neutral 1px hairlines and dividers, table rules, and a blockquote's neutral rule. `check-accent-stripes.ts` enforces this.

## Phase Final: Design QA (after implementation)

The verification authority is `/visual-qa`, not a checklist here. Run it with the screen brief, the `Layout:` line, the required-content list, and, for a changed surface, the `before` set, so it judges the brief, the layout contract, required content, and the before/after pair alongside its platform-guideline checklist on its full capture matrix. Its loop is bounded (its own step sets the round budget), and the round only counts as passed when its dual-reviewer gate clears on fresh evidence. A concrete reference or clone runs reference-fidelity mode.

Defects and flatness count equally, for every brief, not only expressive ones: a bug-free render that reads generic next to the loaded reference is not done. Within the budget, a flatness finding earns another round just as a defect does. Raise the material layering, give the colour a real perceptual ramp (several stops, OKLCH-style), render the hero's focal object as a lit, dimensional thing (a generated bitmap or real light, shadow, and depth; never flat geometric primitives), and add one signature moment, then re-check.

Report done only when visual QA passed on fresh evidence **and** nothing on the surface is still at minimum-effort quality. If the round budget runs out first, report what is still flat or broken as open debt (and record it in `DESIGN.md` accepted debt only if the user accepts it), never as done.

## Final notes

- References are long (200-500 lines). Load two or three, never ten.
- State in one sentence which files you loaded and why.
- If the user pushes back on a routing choice, switch references without arguing.
- If the request is still unclear after two reads, ask one focused either/or question before loading anything.
