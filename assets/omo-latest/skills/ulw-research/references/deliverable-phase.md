Read this when a document deliverable (PDF, DOCX, slides, or standalone HTML) is requested, before the brief is written and again before Phase 5: lane and format interview, Phase 5 final materials, asset and assembly workers, delivery gates, meaning verifier, bounded repair, closing briefing.

# ulw-research - deliverable phase contract

This contract replaces the old blocking format question with a contract that never stalls collection: the deliverable is derived from the request and its destination, the requester is asked only what is genuinely missing, and every promise made about the output is tracked in a manifest until it is delivered, skipped, or blocked with a reason.

Every helper named here is one command of the dispatcher:

```bash
node "$SKILL_DIR/scripts/report-tools.mjs" <command> ...
```

`$SKILL_DIR` is the directory of the skill's `SKILL.md`. The helpers are plain `.mjs` files with no dependencies and run under `node` or `bun` on macOS, Linux, and Windows. They never launch a browser. Exit codes: `0` pass, `1` semantic failure (blockers found, a failed verify, a blocked repair), `2` usage or IO error. JSON goes to stdout and a one-line summary to stderr. Always pass `--session-dir "$SESSION_DIR"` explicitly; `SESSION_DIR` in the environment is only a fallback.

## 1. Lanes

The brief names exactly one lane. The lane decides how much the requester is asked, what the design spec starts from, and how strict the gates are.

| Lane | Definition | How to say it to the requester |
| --- | --- | --- |
| `template-strict` | The requester pointed at a document (a prior report, a page, a house template). Its structure, tokens, citation style, and figure standard are copied mechanically and not reinterpreted. | "I will match the document you pointed at: same structure, colors, fonts, and citation style." |
| `template-vibe` | The requester named a register or feeling ("like a consulting deck", "a clean blog post") without a document to copy. The design spec is written from the defaults in section 6 and the register. | "I will write it in that style using the default report design; point me at an example if you want an exact match." |
| `no-format` | Nothing about the shape was said and the destination implies it (a chat answer, a thread post, a quick note). No interview about format. | "I will answer in the shape this destination expects." |
| `edit-existing` | A complete deliverable for this question already exists (section 2). The run edits it in place. It never regenerates it. | "I will update the existing report rather than write a new one." |

## 2. Deliverable state

Before the brief is written, decide the state of any deliverable the request refers to:

```bash
node "$SKILL_DIR/scripts/report-tools.mjs" outcome state --deliverable <path> --session-dir "$SESSION_DIR"
```

| State | Meaning | What it opens |
| --- | --- | --- |
| `none` | No file, or an empty one. | A fresh run in the lane the request implies. |
| `partial` | The file carries a `STATUS: draft` line in its first 5 lines, or its manifest still has a `pending` row. | Resume the on-disk skeleton: keep the sections that exist, fill the missing ones, and never start over. |
| `complete` | The manifest says every promised row is resolved and the file is present and non-empty. | `edit-existing` only. A complete report is never regenerated from scratch. |

## 3. Destination-derived defaults

The request and its destination usually answer the format question already. Derive the defaults first; the interview (section 4) only covers what this table leaves open.

| Request or destination signal | Default format set | Lane | QA tier |
| --- | --- | --- | --- |
| A question answered in chat, with no document asked for | `post` (the answer itself, Markdown) | `no-format` | light |
| A thread, channel, or messenger post | `post` | `no-format` | light |
| A web page URL as the destination, or "blog", "page" | `html` | `template-vibe` | full |
| The words "PDF", "DOCX", "slides", or "LaTeX" in the request | exactly the named formats (`pdf`, `docx`, `slides`, `latex`) | `template-vibe` | full |
| "Report", "document", "write-up" with no format word and no destination | `pdf` + `docx` | `template-vibe` | full |
| A pointed-at document (a file, a URL, a prior report) | the reference's own format | `template-strict` | full |
| A prior report of the same kind in this workspace, not pointed at | that report's format, offered as the Q1 default | `template-vibe` | full |
| A complete existing deliverable for this question (section 2) | its existing format | `edit-existing` | the original's tier |

QA tiers: **light** never calls `report-tools.mjs`: it rereads the answer once and checks one render of the inline page when one is shown, and records no gates, no `fact verify`, and no manifest. **Full** runs the static gates, the fidelity gate, the layout gates, visual QA of every page, and the proofread gate (section 7).

## 4. The empty-info interview

### Deliverable lane, format, and the empty-info interview

Never block collection on the shape of the deliverable, and never guess it either. Read sections 1-6 of this reference before writing the brief, then:

1. **Derive first.** Name the lane (`template-strict`, `template-vibe`, `no-format`, `edit-existing`) and the promised formats from the request and its destination (section 3). When the request refers to an existing deliverable, decide its state with `node "$SKILL_DIR/scripts/report-tools.mjs" outcome state --deliverable <path> --session-dir "$SESSION_DIR"`: `partial` resumes the skeleton on disk, `complete` means edit-existing and never a regenerated report. `$SKILL_DIR` is this skill's own directory, the folder containing this SKILL.md.
2. **Read the requester's format memory** with the memory tool when the harness exposes one: the pointer `system/human/report-style.md`, then `reference/human-report-style.md`; take the choice recorded for the most similar context (same destination kind and audience) as the first option.
3. **Ask only what is still missing**: at most the three format questions below (destination and format, audience and length, template lineage) in one call to the harness question tool (`ask_user_question` or `request_user_input`), non-blocking where the harness supports it, each with its default first, a free-text "describe the format" path, and "don't care, you decide". Collection starts without waiting; a late answer is folded in until the assembly worker starts, and after that it becomes a re-render request.
4. **Settle source fidelity in the same step.** Seven fields under `## Fidelity` in `brief.md` say what the report must stay faithful to; [fidelity.md](fidelity.md) lists each field's values and default. Infer every field before asking anything; an explicit word in the request decides a field (`answered_by: request`), the destination, the sources, and the Phase 0 axes supply the rest (`answered_by: default`):
   - `as of`: a version, release, tag, commit, date, quarter, "as of", "current", or "latest" in the request decides it; a pointed-at repository anchors on its HEAD or tag at run start, pointed-at documents on their own revisions, and a comparison of two anchors records `multiple: <a>, <b>; rows tagged`. Default `latest at run start, <run date>`, the date of the `$SESSION_DIR` timestamp. Ask when the topic is version-bound or time-sensitive (software, APIs, standards, prices, rates, rankings, laws, programs, schedules) and nothing names an anchor; a stable topic takes the default silently.
   - `meaning check`: the QA tier decides (full: `on`, light: `off`); a chat answer on a legal, medical, financial, procurement, or public-facing question with no speed word is `on`. Ask when two explicit signals disagree: a document deliverable under a literal "quick" or "fast", or a light-tier answer that carries both a speed word and a precision demand; `on` goes first.
   - `source floor`: "official docs only", "peer-reviewed only", "SEC filings" give `primary only` or `classes: <host suffixes>`; "from these only" gives `pointed only: <source ids, URLs, or paths>`; pointed-at sources with no restricting word are seeds, not a floor. Default `standard`. Ask when a restricting phrase sits beside an open-ended question whose answer lives outside that floor. Declare each source's provenance when you snapshot it (`source snapshot --provenance primary|secondary`, by the Phase 3b primary-source rule); `primary only` refuses every source not declared `primary`.
   - `derived numbers`: "compare", "growth", "share", "estimate" give `allowed`; "only the sources' own numbers", "no estimates" give `none`. Default `allowed`. Ask when both kinds of phrase share one request. Record only `allowed` or `none` (any other value stops `fact fill` and `fact verify` with a usage error); a partial or free-text answer records `none`.
   - `languages` (never asked): the report in the user's language or the one they named; sources `en`, plus the user's language for a local topic, plus the pointed-at documents' languages; quotes `original` when every source language equals the report language, else `original with translation beside`.
   - `identifier classes` (never asked): from the Phase 0 axes' domain (software: version strings, package names, commit ids; finance: tickers, filing ids, fiscal periods; law: statute, article, and case numbers; science: gene names, DOIs, trial ids), else `none`. It is recorded only; no gate enforces it yet.
   - `fallback` (never asked): `block-quote`, or `omit` on "drop anything you cannot verify"; no other value is accepted.

   Ask nothing when the request, the destination, and the sources decide every field. Otherwise ask only the fields whose trigger fired, in the same question-tool call as the format questions (`ask_user_question` or `request_user_input`), non-blocking where the harness supports it, each with its default first, a free-text path, and "don't care, you decide" (it resolves to the first option, recorded `answered_by: user`). At most four questions ride in that call: `pending` fidelity fields first, then format Q1, Q2, Q3, then `as of`, `meaning check`, `source floor`, `derived numbers`; a question that loses its slot runs on its default and is named, with that default, in the reply that opens collection. When each reading of a field would make part of the request undeliverable, record it `pending` with no default. Collection starts regardless: `source snapshot`, `fact add`, `fact quote`, `fact derive`, `fact retract`, and `fact export` keep working while answers are pending; only the six reader-facing commands (`fact fill`, `fact verify`, `fact pairs`, `fact meaning-check`, `fact polish-apply`, `fact polish-diff`) and the assembly lane wait for the lock, and those commands exit 1 with `fidelity_interview_incomplete` while a field is `pending` or missing. When the assembly lane is about to start, a field still awaiting its answer keeps its default; a `pending` field is asked once more, blocking this time.
5. **Record it.** Write `## Deliverable` into `brief.md` (lane, state, formats, destination, audience, template, format description, each with `answered_by: user|default|request`) plus `## Fidelity` (the block in [fidelity.md](fidelity.md)), and open the manifest in the same step: `node "$SKILL_DIR/scripts/report-tools.mjs" outcome init --promised <formats> --lane <lane> --session-dir "$SESSION_DIR"`. When the ask is to recreate, reproduce, or rebuild something visual, add `Reference compare: required` with the source (URL or file) and the moments to compare; the comparison runs on every round of the build, not only at the end.

### The three format questions

The interview has three format questions, asked together through the harness question tool without waiting for the answer; the fidelity questions ([fidelity.md](fidelity.md)) ride in the same call only when their triggers fire. A question is **skipped** when the request, the destination, a pointed-at document, or a declared memory episode already answers it; when all three are answered, nothing is asked.

- **Q1 - destination and format.** Options in this order: the memory hit (the choice recorded for the most similar prior context, section 5) when there is one; the derived default from section 3; "describe the format in your own words" (free text, which becomes the brief for the design spec); "don't care, you decide" (resolves to the derived default).
- **Q2 - audience and length.** Who reads it, and a length band: `short` (a page or a post), `standard` (a report of about 5 to 12 pages), or `deep` (a full dossier). The derived default is first.
- **Q3 - template lineage.** The pointed-at document, the last report of the same kind, or the clean analyst register (section 6). The derived default is first.

Non-blocking rule: ask, do not wait, and start collection. Write `brief.md` at once with every field and who answered it; the unanswered fields carry their defaults. A late answer is folded in until the assembly lane starts; after that it becomes a re-render request, handled like any other repair.

Recorded fields, under `## Deliverable` in `brief.md`:

```text
lane: template-vibe
state: none
formats: pdf, docx (answered_by: request)
destination: internal review document (answered_by: default)
audience: engineering leads, standard (answered_by: user)
template: clean analyst register (answered_by: default)
format description: none
```

`answered_by` is one of `user`, `default`, `request`. Run `outcome init` (section 9) in the same step, with the formats as the promised list.

The fidelity fields, the lock, the fidelity question texts, and the `## Fidelity` block grammar are in [fidelity.md](fidelity.md).

Phase 5 opens by turning the recorded fields into `design-spec.md`.

## 5. Report-format memory episodes

When the memory tool is present, the requester's report-format choices are kept as an append-only episode log, so a repeat requester is not asked the same question twice. When no memory tool is present, skip this section silently.

- Pointer: `system/human/report-style.md`, projected every session. One line naming the log, plus the generalizations currently in force.
- Episode log: `reference/human-report-style.md`, one line per episode, newest last:

```text
- [YYYY-MM-DD] <destination kind> · <audience> - <format and template chosen, one line> <!-- src: <session-id>; pattern: observed|declared; confidence: low|medium|high -->
```

Qualification (write after delivery, only when one of these holds): `declared` - the requester stated the preference outright ("always PDF only"); `observed` - the requester made the same unsolicited choice on 2 or more runs (read the log first). Record format and template choices only, never report content, subjects, names, or findings. Append; never rewrite an existing line.

Read-time generalization (before the interview): a similar context is one with the same destination kind and audience. Consistent similar episodes supply Q1's first option; a `declared` similar episode skips the question it answers. One session never flips a default: a single `observed` episode that disagrees with the rest changes nothing.

Pruning: keep the log near 15 lines. When it grows past that, prune the oldest episodes that agree with the majority for their context first. Never prune a `declared` episode or a minority episode (the only one recording a different choice for its context). Update the pointer in the same memory commit when a generalization changes.

## 6. design-spec.md

Write `$SESSION_DIR/design-spec.md` the moment the deliverable fields are known, before any asset lane starts. Every asset and assembly lane receives it.

1. When a document was pointed at, extract it first. Do not retype its stylesheet:
   ```bash
   node "$SKILL_DIR/scripts/report-tools.mjs" format-extract <reference.html|reference.md> --out "$SESSION_DIR/design-spec.md"
   node "$SKILL_DIR/scripts/report-tools.mjs" format-extract <https://...> --from-url --out "$SESSION_DIR/design-spec.md"
   ```
   The extractor copies the CSS custom properties (light and dark), font stacks by role, the most-used colors, px breakpoints, the section skeleton, the heading numbering, the citation marker style, the lineage mode, and the figure containers, under the fixed sections `Extracted from`, `Tokens`, `Typography`, `Layout`, `Structure`, `Figures`, `Citations`, `Open questions`. A field it cannot determine reads `TODO: ask`. A PDF reference is unsupported: ask for its HTML or Markdown source.
2. Fill every `TODO: ask` from the interview answers, then from the clean analyst register below. Never guess a value.
3. Apply the binding figure standard and the defaults:
   - Every figure sits in a fixed-size container with a caption; the image scales inside it with contain-fit, its aspect ratio preserved, never stretched, cropped, or spilling out. Every chart carries a title, axis labels, units, and value labels as real text, with deterministic ticks (round values computed from the data range, never left to a renderer's auto-scaling). A chart is plotted only from a `fact export --csv` file saved under `$SESSION_DIR/assets/`, its figure carries `data-chart-source` pointing at that file, and its axis tick labels sit inside one `<g class="ticks" data-tick-step=".." data-tick-min=".." data-tick-max="..">` group, so the fidelity gate can check every value label against the ledger and every tick against the declared grid.
   - One accent color over neutral background, text, and rule tones. Every color in prose, charts, and diagrams is a spec token.
   - For web deliverables, HTML/CSS bars and inline SVG are preferred over image charts, and the page has responsive breakpoints so it reads on a phone, a tablet, and a desktop.
   - Gothic (sans) type only, as a real embedded webfont for the report language; Korean text uses `word-break: keep-all; overflow-wrap: anywhere`.
   - A closing section on how the report was made (method, sources, members and waves, the gates it passed, what verification overturned) and a numbered sources section with access dates.
4. Declare the lineage mode on its own line in the spec:
   - `Lineage: inline` (the analyst default): every unit-bearing number carries `MEASURED`, `ASSUMED`, or `DERIVED` inline or in a `data-lineage` attribute, or a `[S<n>]` citation in the same element. The gate reports an untagged number as a major defect.
   - `Lineage: section` (web and blog register): sources are bracketed source-label links at sentence end (the report language's word for "source"), lineage distinctions live in the figure captions, and every section is cited. The gate reports an untagged number as an advisory minor defect; the section-citation gate carries the weight.

## Phase 5 — Final materials

The promised formats recorded in `brief.md` and `outcome.json` are binding; `pdf` + `docx` is the default pair only when a document was asked for with no format word and no destination:

| Target | How |
|---|---|
| PDF (default) | Author the report as one self-contained HTML file, then print it headless: `chrome --headless --disable-gpu --no-pdf-header-footer --print-to-pdf=<out.pdf> file://<report.html>`. Embed the `design-spec.md` fonts as real webfonts (CJK included) instead of trusting system fallbacks. `uv run --with weasyprint python` is the fallback renderer. |
| DOCX (default) | `pandoc <report.md> -o <out.docx>`, adding `--reference-doc=<template.docx>` when the user has a house style; `uv run --with python-docx python` when pandoc is unavailable. Charts and Mermaid renders go in as images. |
| Slides / deck | `uv run --with python-pptx python` — one claim per slide, a chart or diagram per claim. |
| Standalone HTML / Markdown | The authored source itself. |

**Every reader-facing edition is filled and verified.** The main report, the inline edition, LaTeX sections, and slide sources are all filled by `fact fill` from the same placeholder source, and each is verified on its own by `fact verify`; PDF and DOCX are renders of a verified source (`pandoc <report.html> -o <out.docx>` when the DOCX comes from the filled HTML). Fill and verify HTML or Markdown sources (`fact verify --format html|md`, with a fill map for Markdown); verify LaTeX through its Markdown source, since direct `--format tex` verification exits 2. Record a delivered edition with `outcome set <format> delivered --path <file> --verified-by <record>`, where `<record>` is the `fact verify` output whose `artifact_sha256` equals the sha256 of the delivered source; a PDF or DOCX adds `--rendered-from <source>` and is matched against that source. `outcome verify` refuses a delivered row without a matching record (`edition_unverified`).

**Write `design-spec.md` the moment the deliverable is recorded — before any asset worker spawns.** When a document was pointed at, extract it instead of retyping its stylesheet: `node "$SKILL_DIR/scripts/report-tools.mjs" format-extract <reference> --out "$SESSION_DIR/design-spec.md"` (`--from-url` for a URL), then fill every `TODO: ask` from the interview answers, never by guessing. It is the one design contract every asset and assembly worker receives: template family (absent a reference, the clean analyst-report register — one accent over neutral tones, generous margins, styled section headings, no emoji, no clipart), palette tokens, body/heading fonts (a real gothic CJK webfont — Pretendard, Noto Sans KR — when the report language needs one, with `word-break: keep-all` for Korean), responsive breakpoints and HTML/CSS charts for web deliverables, the figure standard below, and the lineage mode on its own line: `Lineage: inline` (analyst default: every unit-bearing number tagged MEASURED / ASSUMED / DERIVED or cited in the same element) or `Lineage: section` (web and blog register: every section cited, lineage in the figure captions). Section 6 lists the full defaults. One font family and one palette govern prose, charts, Mermaid, and generated images alike; a diagram rendering in a random default font inside a styled report is a defect, not a style choice.

**The figure standard — binding for every image, chart, and diagram.** Each figure sits in a fixed-size container styled from the spec (border, background, caption); the image scales to fit entirely inside it with its original aspect ratio preserved — object-fit: contain semantics — never stretched, never cropped, never spilling out. Every chart carries a title, axis labels, units, and value labels in the report's language; a bare number the reader cannot name is a defect.

Asset workers (background, parallel, each fed `design-spec.md`) — visuals are the DEFAULT deliverable of this phase, not garnish the user must ask for; a delivered report without figures is an incomplete run:

- **Charts for every quantitative finding, computed from real data.** Pull the numbers into an actual table first (CSV/JSON under `$SESSION_DIR`), then plot from that table, never from prose. Follow the data-scientist tool doctrine — numpy always, Polars for filtering/sorting/transforms, DuckDB for joins/aggregations/window functions, never pandas — and load the `data-scientist` skill when this session has it: `uv run --with numpy --with polars --with duckdb --with pyarrow --with matplotlib python`. Keep `pyarrow` in that set — the DuckDB-to-Polars handoff (`.pl()`) fails without it, and `.df()` fails without pandas, so hand data across through `.pl()`, never `.df()`. Save to `$SESSION_DIR/assets/`. Plot ONLY from the ledger: export the rows with `node "$SKILL_DIR/scripts/report-tools.mjs" fact export --csv --out "$SESSION_DIR/assets/<chart>.csv" --session-dir "$SESSION_DIR"` and draw from that CSV; every chart figure carries `data-chart-source="assets/<chart>.csv"`, and its axis tick labels sit inside a `<g class="ticks" data-tick-step=".." data-tick-min=".." data-tick-max="..">` group, so the fidelity gate can check every plotted value and tick (section 6 tick rule).
- **Mermaid graphs** for process, architecture, argument, timeline, and evidence-flow structure, themed to the spec's fonts and palette. Render each to SVG and confirm the file exists before the document references it.
- **Generated visuals through the imagegen skill whenever the session has it:** a cover plus a concept illustration per major theme, prompted from the spec's style, palette, and mood — document-styled illustration, never generic stock art dropped into a designed page.
- **Full-page screenshots** of the top 5-10 sources, saved by the web workers while reading, as provenance you can show.

**Every final report is visual — binding.** It carries at least one data visualization of the run's own findings (a chart drawn from the data table, next to the claim it supports), a diagram wherever structure matters, and the topic's own screenshots and video, embedded or linked with attribution; every figure is captioned, sourced, and given alt text. Mermaid for flows, timelines, and relations; generated charts for numbers; image generation only where an illustration genuinely helps; PlotNeuralNet for paper-format reports that describe model architectures. Every asset lane reads [report-visualization.md](report-visualization.md) first: which tool fits which finding, the render commands, the quality bar, and the checklist the gates apply. A light-tier post still carries one visual when it reports numbers or structure.

**Visual reproductions are compared against the source — binding.** When the ask is to recreate, reproduce, or rebuild something visual (a clip in 3D, an interface from a screenshot, a figure), real frames or captures from the source are the reference: extract them at the moments that define the source, render the output at the same moments and viewpoints, compose side-by-side pairs, critique every pair by looking at it, fix the largest differences, and compare again within the repair bound. The final pairs, and the first-round pairs in the correction log, go into the report. Procedure: [report-visualization.md](report-visualization.md), section 3, Reference compare. A reproduction checked only against itself is unverified.

**Verify the asset manifest before rendering.** List every asset the document references, assert each file exists and is non-empty on disk, and re-render whatever is missing. A document that renders with three broken diagrams is a document you will publish twice.

Assembly worker — `task(category="deep-low", load_skills=["frontend", "visual-qa", "visualize", "data-scientist", "ulw-loop"], run_in_background=true, ...)`: before writing, read every available design and visualization skill and apply it — the report is a designed artifact, not a text dump; the worker's prompt carries `design-spec.md`. Use the template the user approved; absent a stronger house style the default skeleton is executive summary → key findings by theme → detailed analysis (quotes under 20 words with attribution, charts, Mermaid graphs, generated visuals, SHA-pinned permalinks, verification results) → comparative analysis when options compete → numbered sources with access dates → methodology appendix (workers, waves, searches, verifications, debate rounds) → correction log naming what verification overturned. Write it long and specific: every claim cites `[Source N]`, and the sources section lists every source the run actually used rather than a curated few. The assembly worker writes `report.src.html` with placeholders, never typed values; the orchestrator runs `node "$SKILL_DIR/scripts/report-tools.mjs" fact fill "$SESSION_DIR/report.src.html" --out "$SESSION_DIR/report.html" --session-dir "$SESSION_DIR"` and gates the filled file. The inline edition for a PDF, DOCX, or oversized HTML deliverable comes from the same source (`fact fill "$SESSION_DIR/report.src.html" --inline --out "$SESSION_DIR/report.inline.html"`, which inlines local stylesheets and makes image paths absolute on the filled output), never from a hand copy.

### The delivery gates — every gate for the QA tier must PASS, in order

Nothing reaches the user until the gates pass. Record each result with `node "$SKILL_DIR/scripts/report-tools.mjs" outcome gate <static|fidelity|layout|visual|proofread> <pass|fail|not_run> --session-dir "$SESSION_DIR"`; a gate this harness does not provide is recorded `not_run --reason "<why>"` (`fidelity not_run` refuses without the reason). A chat or post answer (light tier) calls no `report-tools.mjs` command and records no gate; every paginated or web deliverable runs them all (sections 3 and 7).

1. **Static gates (always, before anyone looks at pixels).** `node "$SKILL_DIR/scripts/report-tools.mjs" check "$SESSION_DIR/report.html" --design-spec "$SESSION_DIR/design-spec.md" > "$SESSION_DIR/defects.json"`: keep-all, palette tokens, emoji, em dashes, heading length, unsourced numbers, figure containers, chart text, a report with no visual, images without alt text, gothic-only fonts, citations, the closing and sources sections, broken assets. Codes and fixes: [report-gates.md](report-gates.md).
2. **Fidelity gate.** `node "$SKILL_DIR/scripts/report-tools.mjs" fact verify "$SESSION_DIR/report.html" --session-dir "$SESSION_DIR"` checks every number, quote, chart value, and sentence binding against the ledger, the snapshots, `claims.json`, and the locked `## Fidelity` block of `brief.md`, and writes `$SESSION_DIR/fidelity-defects.json` with the verified file's `artifact_sha256`. Exit `1` on any blocker or major defect: record `outcome gate fidelity fail` and repair; minors alone exit `0` and are listed. Verify every other edition the same way into its own record (`--out "$SESSION_DIR/fidelity-inline.json"`). Codes and fixes: [report-gates.md](report-gates.md); grammar: section 11, Fidelity gate.

**Meaning verifier (a separate session reads every sentence against its source).** When the brief's `meaning check` is `on`, run it after the fidelity gate passes; when it is `off`, `fact pairs` writes `{"skipped": true}` and the step ends there. `node "$SKILL_DIR/scripts/report-tools.mjs" fact pairs "$SESSION_DIR/report.html" --session-dir "$SESSION_DIR"` writes `$SESSION_DIR/pairs.json`: one pair per distinct source quote a findings sentence cites, per sentence bound only by `data-claim` (against the claim's statement), and per `data-translates` span, with `force_strong` set where the lexicon or the hedge position says a strong reader must look. Then spawn one reader worker on the cheapest category, `task(category="quick", ...)`, whose prompt carries ONLY the path of `pairs.json` and the verdict schema, in exactly this text:

```text
TASK: meaning reader, <cheap|strong> tier. Read ONLY the pair file <SESSION_DIR>/<pairs.json or pairs-strong.json> and the verdict schema in this prompt. Do not open, search, or fetch any other file, page, or tool output: not the report, the ledger, the sources, or the claim graph.
For each pair, compare `sentence` with `quote` (with `statement` for a claim pair) and decide whether the sentence says something its source does not.
Write <SESSION_DIR>/meaning-<cheap|strong>.json as {"model_tier": "<cheap|strong>", "verdicts": [...]} with one verdict per pair_id: {"pair_id": "P<n>", "verdict": "ok" or "drift", "drift_kind": ..., "fragment": ..., "source_fragment": ..., "explanation": "<one sentence>"}.
drift_kind is exactly one of: subject_changed, scope_widened, hedge_dropped, certainty_added, causality_added, direction_flipped, population_changed, evidence_grade_raised, other.
fragment is copied verbatim from the sentence and source_fragment verbatim from the quote or statement: the shortest exact substrings that show the drift, never paraphrased, never empty. A verdict whose fragments are not found word for word is discarded.
A sentence that shortens, summarizes, or translates its source without changing subject, scope, certainty, causality, direction, population, or evidence grade is ok. Do not rewrite anything; return verdicts only.
```

Next, write `$SESSION_DIR/pairs-strong.json` (the same file, keeping only the pairs the cheap reader flagged plus every `force_strong` pair) and spawn one reader on the strong category, `task(category="ultrabrain", ...)`, with the same prompt for the strong tier. Then `node "$SKILL_DIR/scripts/report-tools.mjs" fact meaning-check --pairs "$SESSION_DIR/pairs.json" --verdicts "$SESSION_DIR/meaning-cheap.json" --verdicts "$SESSION_DIR/meaning-strong.json" --edition html --session-dir "$SESSION_DIR"` writes `$SESSION_DIR/fidelity-meaning-defects.html.json`, the meaning record of that edition (repeat `fact pairs` and the readers with `--edition <format>` for every other delivered edition whose source bytes differ): both tiers naming the same `drift_kind` (never `other`) is `meaning_drift_confirmed` (major, exit `1`); one tier alone is `meaning_drift_warn` (minor); a verdict whose fragments are not in the pair is dropped and counted. Repair every confirmed drift by rewriting the sentence from its quote, re-fill, re-verify, and re-read only the changed sentences (`fact pairs ... --changed-since <the new fill map>`); or, when the sentence is right, dispute it with `--dispute P<n> --reason "<why>"`, which keeps the defect for this run and logs the dispute. Pass the record to `repair decide` as one more `--defects` file and list the warnings in the closing briefing. Each flagged pair adds one line to `$SESSION_DIR/fidelity-calibration.jsonl`, whose header records the promotion criterion: `meaning_drift_confirmed` moves from major to an integrity code only after at least 20 runs with disputed-to-confirmed below 10 percent and no more than 2 report-level false blocks in the last 20 runs. These are starting values, changed only with evidence; the scripts never call a model.

3. **Layout gates.** Print the probe with `node "$SKILL_DIR/scripts/report-tools.mjs" layout-probe --json`, evaluate its `source` in the rendered page through the `browser` skill's owned headless engine, save the result as `$SESSION_DIR/boxes.json`, and re-run `check` with `--layout "$SESSION_DIR/boxes.json"` (overflow, clipped text, distorted images, overlapping siblings).
4. **Visual QA.** Render the produced artifact back to images — PDF pages to PNG, the HTML through the browser skill at desktop and phone widths — and look at them: missing or broken figures, images stretched or spilling their containers, diagram or chart text rendered off the spec's font or palette, clipped tables, overflowing CJK text, blank pages, unlabeled chart values, wrong page breaks. Reading the source markup is not visual QA; inspect the pixels.

**Repair is bounded.** After every gate run that found defects, ask `node "$SKILL_DIR/scripts/report-tools.mjs" repair decide --state "$SESSION_DIR/repair-state.json" --defects "$SESSION_DIR/defects.json" --defects "$SESSION_DIR/fidelity-defects.json" --artifact-bytes <bytes> --renders <pages> --session-dir "$SESSION_DIR"` and obey it: `repair` means fix and re-run; `deliver` means stop and ship with the residual defects the manifest now lists; `block` means a content-integrity defect remains (or there is no usable artifact), so do not deliver: tell the user what blocks it and record the row as `failed` with the reason. Never repair past a stop decision (3 attempts, a 2-attempt plateau, an oscillation, or 15 minutes; section 8). Fidelity defects ride the same loop (`--defects "$SESSION_DIR/defects.json" --defects "$SESSION_DIR/fidelity-defects.json"`), but a `deliver` decision does not release them: while `fidelity-defects.json` lists `untraceable_number`, `value_mismatch`, `unit_mismatch`, `quote_not_verbatim`, `quote_unbound`, `unbound_sentence`, or `chart_value_untraceable`, `outcome verify` fails with `fidelity_unresolved` and `outcome finish` refuses, even with the repair budget spent. The only ways out are a ledger row, an `assumed` row with its rationale, or removing the sentence.

Then resolve every manifest row (`outcome set <format> delivered --path <file> --verified-by <record>` (plus `--rendered-from <source>` for a PDF or DOCX) or `blocked_capability` / `skipped` / `failed` with `--reason`) and deliver. **Show the report inline first when the harness can.** When this session has a tool that shows an HTML page inline in the conversation (`show_html_page`, `html_render`), the requester's first view of the result is the report itself: the gated `report.html` when it fits the tool, otherwise an inline edition built from the same synthesis and assets, with the reference-compare pairs when the run has them (section 9, Showing the report inline). Then give the artifact paths and a compact chat-readable summary of what the report says — the answer in a few sentences, the numbers that matter, and what to look at first. This holds on every tier, a chat answer included, and when the thing built is itself a page (an app, a 3D scene), showing it does not replace the report page. Without an inline page tool, deliver the artifact paths plus that summary. The document is the deliverable; the summary is what gets it read.

### The closing briefing — every run ends with it

The last thing the user reads states, in one compact block, what the answer is made of:

- **The printed block.** `node "$SKILL_DIR/scripts/report-tools.mjs" outcome verify --session-dir "$SESSION_DIR" && node "$SKILL_DIR/scripts/report-tools.mjs" outcome finish --session-dir "$SESSION_DIR" && node "$SKILL_DIR/scripts/report-tools.mjs" outcome briefing --session-dir "$SESSION_DIR"`: sources and distinct domains counted from `sources-ledger.md`, elapsed minutes from the session directory's own timestamp, every promised deliverable with its status, the gate results, the residual defects, and the repair summary. A failing `verify` means the run is not done.
- **Effort.** Workers, waves, excursions, and verifications — the same counters as the `SYNTHESIS.md` header — plus how many sources were primary and how many claims went to the unresolved/refuted annex. `fact export` also prints a coverage line (`N of M sources snapshotted, by extractor: ...`); paste it into the briefing's effort line.

Never ship the artifact without this block, and never fill it from memory — paste the printed block and read the rest off the journal.

**Record the format choice.** After delivery, when the harness exposes a memory tool and the run qualifies (the user declared a preference, or made the same unsolicited choice on 2+ runs), append one episode line to `reference/human-report-style.md` in the grammar of section 5, and update the `system/human/report-style.md` pointer when a generalization changed.
Team disband: see the `SKILL.md` failure-modes table.

## 7. Delivery gates, in order

Nothing reaches the requester until the gates for its QA tier pass, in this order. Each gate writes its status into the manifest with `outcome gate <gate> <pass|fail|not_run> --session-dir "$SESSION_DIR"`.

1. **Static gates.** `check` on the rendered HTML:
   ```bash
   node "$SKILL_DIR/scripts/report-tools.mjs" check "$SESSION_DIR/report.html" --design-spec "$SESSION_DIR/design-spec.md" > "$SESSION_DIR/defects.json"
   ```
   It checks keep-all, palette tokens, emoji, em dashes in prose (en dashes outside numeric ranges are advisory), heading length and generic headings, unsourced numbers (section 6), figure containers and captions, chart SVG text, a report with no visual at all, images without alt text, Korean serif fonts, sections without citations, the closing section, the sources section, broken local assets, and any `--require-section <regex>` the brief requires. Every code, severity, and fix is in [report-gates.md](report-gates.md).
2. **Fidelity gate.** The fidelity verifier checks every number, quote, and claim binding against the fact ledger and the source snapshots; its codes are the Fidelity Gate table in [report-gates.md](report-gates.md). It runs before layout and visual QA because a content failure would otherwise force an expensive re-render.
   ```bash
   node "$SKILL_DIR/scripts/report-tools.mjs" fact verify "$SESSION_DIR/report.html" --session-dir "$SESSION_DIR"   # writes fidelity-defects.json
   ```
   Exit `1` on any blocker or major defect: record `outcome gate fidelity fail`; minors alone exit `0` and are listed. This stage verifies HTML and Markdown with its fill map; verify LaTeX through its Markdown source (section 11).
3. **Layout gates.** Print the probe, evaluate it in the rendered page through the browser skill's owned headless engine, save the returned object as `$SESSION_DIR/boxes.json`, and re-run `check` with `--layout "$SESSION_DIR/boxes.json"`. It adds overflow past the container, clipped text, scroll containers, distorted images, and overlapping siblings.
   ```bash
   node "$SKILL_DIR/scripts/report-tools.mjs" layout-probe --json   # .source is one async expression
   ```
4. **Visual QA.** Render the artifact to images (PDF pages to PNG, the HTML in the browser skill's engine at desktop and phone widths) and look at the pixels for what a machine cannot see: composition, hierarchy, page breaks, figure legibility.
5. **Proofread.** A dedicated writing lane proofreads the final text only (grammar, spelling, punctuation, terminology, native register); it never composes or rewrites content and never edits the file. Edits change text only; an edit whose before/after would alter any tag, attribute, comment, or script/style content is rejected, and markup changes go through the full fidelity gates instead. It returns `polish-edits.json` (`[{before, after, reason}]`), which the orchestrator applies edit by edit; a rejected or unlocated edit goes back to the lane with its reason, within the repair budget. The backstop diff, `fact verify` on the polished file, and the meaning readers on the pairs whose sentences polish changed run before the fidelity gate is recorded again. Edits must not span entities (such as &amp; or &lt;) or markup; narrow the edit to the surrounding words instead, and a rejected edit comes back with the reason to act on.
   ```bash
   node "$SKILL_DIR/scripts/report-tools.mjs" fact polish-apply "$SESSION_DIR/report.html" --edits "$SESSION_DIR/polish-edits.json" --out "$SESSION_DIR/report.polished.html" --session-dir "$SESSION_DIR"
   node "$SKILL_DIR/scripts/report-tools.mjs" fact polish-diff --before "$SESSION_DIR/report.html" --after "$SESSION_DIR/report.polished.html" --session-dir "$SESSION_DIR"   # writes polish-defects.json
   node "$SKILL_DIR/scripts/report-tools.mjs" fact pairs "$SESSION_DIR/report.polished.html" --changed-since "$SESSION_DIR/report.html.fill-map.json" --session-dir "$SESSION_DIR"
   ```

**Every reader-facing edition is verified.** The main report, the inline edition (section 9), LaTeX per-section sources, and slide sources are filled by `fact fill` from one placeholder source and each is verified by `fact verify` into its own record; PDF and DOCX are renders of a verified source. A delivered row is recorded with `outcome set <format> delivered --path <file> --verified-by <record>`, where the record's `artifact_sha256` equals the sha256 of the delivered source; a PDF or DOCX adds `--rendered-from <source>` and is matched against that source. Each delivered edition also carries its own meaning record: run `fact pairs` on that edition's source and `fact meaning-check --edition <format>`, which writes `fidelity-meaning-defects.<format>.json` bound to those bytes (section 7). The barrier binds each edition to a record for its current bytes: every record whose `artifact_sha256` equals those bytes is a candidate (its own record, another edition's record for a byte-identical copy or the html edition a PDF renders, and the legacy single file `fidelity-meaning-defects.json`), and the one with the newest `written_at` wins; on equal times the more severe verdict wins (`meaning_drift_confirmed`, then `meaning_unverified`, then clean). In a session with a fact ledger, `outcome verify` refuses a delivered row without a matching record (`edition_unverified`, blocker, integrity). Markdown and LaTeX verification arrive with the Markdown stage; until then `fact verify --format md|tex` exits `2`, and such an edition is recorded `blocked_capability` with that reason rather than delivered unverified.

The light tier calls no `report-tools.mjs` command and records no gate; it rereads the answer and checks one render of the inline page when one is shown. A full-tier gate this harness does not provide is recorded `not_run` (`fidelity not_run` needs `--reason "<why>"`), and `fact verify` checks HTML and Markdown editions. The full tier runs all five.

## 8. Bounded repair loop

After every gate run that produced defects, ask the tracker what to do next:

```bash
node "$SKILL_DIR/scripts/report-tools.mjs" repair decide --state "$SESSION_DIR/repair-state.json" --defects "$SESSION_DIR/defects.json" --defects "$SESSION_DIR/fidelity-defects.json" --artifact-bytes <report bytes> --renders <rendered page count> --session-dir "$SESSION_DIR"
```

`--defects` repeats: every file (a `check` result, a `fact verify` record, or a bare defect array) joins one defect list, so one decision covers both gates.

- `action: repair`: fix the listed defects, re-render, re-run the gate, and ask again.
- `action: deliver`: stop repairing and deliver. `reason` is `clean` or the stop condition that fired; the residual defects are written into the manifest and listed in the closing briefing.
- `action: block` (exit `1`): do not deliver. Either a content-integrity defect remains (`missing_closing_section`, `missing_citation_section`, `broken_asset_reference`, `missing_required_section`, `missing_promised_deliverable`, or a fidelity integrity code: `unresolved_placeholder`, `value_mismatch`, `unit_mismatch`, `quote_not_verbatim`, `claim_status_invalid`, `incompatible_operands`, `pointer_label_mismatch`, `quote_anchor_unresolved`, `unit_duplicated`, `polish_diff_fail`, `fidelity_interview_incomplete`, `derived_not_allowed`, `source_outside_floor`, `brief_changed`, `snapshot_hash_mismatch`, `chart_value_untraceable`, `quote_unbound`, `edition_unverified`), or a stop fired without proof of a usable artifact (`--artifact-bytes` and `--renders`). Tell the requester what blocks delivery and record the affected rows as `failed` with the reason.

**The fidelity barrier is separate from the budget.** A `deliver` decision does not release fidelity defects: `outcome verify` and `outcome finish` use the fidelity gate's `blocksDelivery` predicate over `DEFECT_CODES` (integrity, blocker, or major), never a CLI code list or the row's claimed severity. Unknown codes refuse with `unknown defect code <code>`; known blockers produce `fidelity_unresolved`. Fix the defect and re-verify the edition before delivery. The table and the record schema in `scripts/fidelity-records.mjs` are the spec for the barrier. Every fidelity record carries one envelope (`record_kind`, `artifact_sha256`, `report`, `written_at`, `defects`) plus the fields its kind declares; a record with a missing, unknown, or ill-typed field refuses as malformed (`edition_unverified`). `FIDELITY_RECORDS` has one row per record kind, each rule a column: where its records are, when a delivered session must carry one, the envelope field that binds it to the delivered bytes (`artifact_sha256`), and which defects block. The rows are `fidelity` (`fact verify`, one record per delivered edition, named by `--verified-by`, plus every existing session fidelity record even when unattached) and `meaning` (`fidelity-meaning-defects.<format>.json` from `fact meaning-check --edition <format>`, one per delivered edition, required while the locked brief's `meaning check` is `on` or its `## Fidelity` block is malformed; missing: `meaning gate not run`, the same `edition_unverified` refusal, released only by the logged `fidelity not_run` exemption in a session with no ledger or fidelity record). An edition with no record of its own uses any edition meaning record for the same bytes (a byte-identical copy at another path, or a PDF of the html edition, shares the html record), else the legacy session record `fidelity-meaning-defects.json`, so a single-edition session written before per-edition records still delivers unchanged; an edition with none refuses with `meaning gate not run` naming the file to write. A record whose `artifact_sha256` does not match its edition's source bytes refuses that edition alone as `is for a different edition`, so a meaning record from an earlier draft never passes a rewritten one: rerun `fact pairs` and the readers after every rewrite. A `meaning_drift_confirmed` or `meaning_unverified` in that record holds the barrier like any other major.

**After a dispute.** `--dispute P<n> --reason` records the disagreement in the calibration log; it does not clear the defect, so `fact meaning-check` keeps exiting `1` and the barrier keeps holding by design. Proceed through `repair decide` with the edition's meaning record (`fidelity-meaning-defects.<format>.json`) among its `--defects` (the dispute stays recorded on that run): the writer changes the sentence (rewrite it from the quote, or `fact fill --replace-failed` for a sentence that failed twice), then rerun `fact pairs` and the readers. There is no path that delivers a disputed `meaning_drift_confirmed` sentence unchanged. Only a newer reader run on the same bytes replaces the verdict: among the records for an edition's current bytes (its own, another edition's record for the same bytes, the legacy `fidelity-meaning-defects.json`) the one with the newest `written_at` holds the barrier, whichever file it is in, and on equal times the more severe verdict holds it. A drift confirmed after the edition's own clean run keeps blocking, and a drift verdict stops holding the barrier once a later reader run on those bytes judges the sentence again.

Defaults: 3 repair attempts, a plateau of 2 consecutive attempts that do not improve the (blocker, major, minor) counts compared in that order, and 15 minutes of wall clock from the first decision. A defect set that repeats an earlier attempt exactly stops the loop as an oscillation. When several conditions hold at once the reported reason follows `clean`, `blocking`, `oscillation`, `plateau`, `max_attempts`, `wall_clock`.

## 9. Outcome manifest and closing briefing

`$SESSION_DIR/outcome.json` tracks every promise made about the output, so a promised format is never silently dropped.

```bash
node "$SKILL_DIR/scripts/report-tools.mjs" outcome init --promised pdf,docx --lane template-vibe --session-dir "$SESSION_DIR"
node "$SKILL_DIR/scripts/report-tools.mjs" outcome set pdf delivered --path "$SESSION_DIR/report.pdf" --rendered-from "$SESSION_DIR/report.html" --verified-by "$SESSION_DIR/fidelity-defects.json" --pages 12 --session-dir "$SESSION_DIR"
node "$SKILL_DIR/scripts/report-tools.mjs" outcome set docx blocked_capability --reason "no document converter in this session" --session-dir "$SESSION_DIR"
node "$SKILL_DIR/scripts/report-tools.mjs" outcome render "$SESSION_DIR/renders/page-01.png" --page 1 --session-dir "$SESSION_DIR"
node "$SKILL_DIR/scripts/report-tools.mjs" outcome verify --session-dir "$SESSION_DIR"
node "$SKILL_DIR/scripts/report-tools.mjs" outcome finish --session-dir "$SESSION_DIR"
node "$SKILL_DIR/scripts/report-tools.mjs" outcome briefing --session-dir "$SESSION_DIR"
```

- `init` runs when the brief is written (section 4): one `pending` row per promised format (`pdf`, `docx`, `html`, `md`, `latex`, `slides`, `post`).
- `set` resolves a row: `delivered` (absolute path; bytes are read from disk; pages when paginated), `blocked_capability` (this session cannot produce it), `skipped` (the requester or the lane dropped it), or `failed`. Every status except `delivered` needs `--reason`; `outcome set <format> delivered` records intent permissively, while the required `outcome verify` and `outcome finish` enforce the delivery barrier.
- `verify` runs before delivery and exits `1` while a row is `pending`, a status lacks its reason, a delivered file is missing or empty, a registry blocker remains (`fidelity_unresolved`), or a delivered edition has no matching verify record (`edition_unverified`, section 7). Delivered HTML/PDF/DOCX without any ledger or record also refuse with `fidelity gate not run`; explicitly record `outcome gate fidelity not_run --reason "<text>"` to exempt an unverified edition. With `meaning check: on`, or a malformed `## Fidelity` block (which cannot prove the check off), a delivered edition without a meaning record (bound to the newest record for its current bytes, whether its own `fidelity-meaning-defects.<format>.json`, another edition's record for the same bytes, or the legacy single file `fidelity-meaning-defects.json`; on equal times the more severe verdict) refuses with `meaning gate not run` under the same rule; a meaning record for other bytes than its edition's refuses that edition as `is for a different edition`. This choice is logged in `gateHistory` with its timestamp and reason; it never releases an existing ledger requirement, record, or defect, and a subsequent fidelity gate status replaces the exemption.
- `finish` refuses (exit `1`, nothing stamped) while the fidelity barrier holds.
- `finish` stamps the finish time, the elapsed minutes from the manifest's `startedAt` (the run clock `outcome init` recorded once: `--started-at`, else a supervisor's `terminal-state.json` `started_at`, else the session directory's timestamp name; a value after `--now` or more than a day from a timestamped name is refused), and the source and domain counts from `sources-ledger.md`.
- `briefing` prints the closing block from the manifest and the ledger: sources and distinct domains, elapsed minutes, every deliverable with its status, the gate results, the residual defects, and the repair summary. That text is the closing briefing, pasted as printed, never filled in from memory.

### Showing the report inline

When the session has a tool that shows a self-contained HTML page inline in the conversation (`show_html_page` in OmO, `html_render` on hosts that offer it), the requester sees the report there first, after `verify` passes and before the closing briefing. A path to a file the requester has to go and open is not the first thing they read.

- **Which page.** The gated `report.html` itself when the promised deliverable is a self-contained HTML report within the tool's size limit. Otherwise (a PDF, DOCX, LaTeX, or slide deliverable, or an HTML report over the limit) build an inline edition from the same `SYNTHESIS.md` and the same assets: the answer, the headline figures, the reference-compare pairs when the run has them, the key findings with their citations, the numbered sources, and the paths of the promised artifacts. Run `check` on the inline edition like any HTML deliverable.
- **Self-contained.** No network: styles and scripts inline, no remote fonts, no CDN. Pass local images as absolute file paths (the tool inlines them) or as data URIs. Render it once at the reply width and at 390 px before showing it, and pass the measured height. When the session has the `visualize` skill, follow its inline-page constraints.
- **Every tier, and always the report.** A light-tier chat answer with no promised document still ends with an inline page: the answer, its figures, the reference-compare pairs, and the sources. When the thing built is itself a page (an app, a 3D scene, a demo), show it too, but it never stands in for the report page, and a page that only links or redirects to a file is not a report.
- **Not a manifest format.** The inline page never replaces or resolves a promised format; every manifest row still ends `delivered`, `blocked_capability`, `skipped`, or `failed` with its own artifact.
- **No inline tool.** Deliver the artifact paths and the chat summary as before.

## 10. Command reference

Invoke every command as `node "$SKILL_DIR/scripts/report-tools.mjs" <command>`. Exit codes: `0` pass, `1` semantic failure, `2` usage or IO error. `fact polish-apply` deliberately permits partial application: exit `0` means the output was written, not that every edit applied. Check its top-level JSON `applied` boolean (`false` if any edit was rejected or unlocated); `data.applied` remains the accepted-edit count.

| Command | Flags | Exit `1` when |
| --- | --- | --- |
| `check <report.html>` | `--design-spec <spec>` (required), `--layout <boxes.json>`, `--require-section <regex>` (repeatable), `--max-defects N` | any blocker defect |
| `layout-probe` | `--cap N` (default 400), `--root <selector>` (default `main, article`), `--json` | never |
| `repair decide` | `--state <file>` and `--defects <json>` (required; repeatable, the files are concatenated), `--artifact-bytes N`, `--renders N`, `--session-dir <dir>`, `--max-attempts N`, `--plateau-limit N`, `--wall-clock-ms N`, `--now <ISO>` | the decision is `block` |
| `outcome init` | `--promised <list>` (required), `--lane <lane>`, `--session-dir <dir>`, `--now <ISO>`, `--started-at <ISO>` | never |
| `outcome set <format> <status>` | `--path <file>`, `--pages N`, `--reason <text>`, `--verified-by <fidelity record>`, `--rendered-from <source>`, `--session-dir <dir>`, `--now <ISO>` | never |
| `outcome gate <gate> <status>` | `--session-dir <dir>`, `--now <ISO>`, `--reason <text>` (required for `fidelity not_run`; logged in `gateHistory`) | never (a missing reason exits `2`) |
| `outcome render <png>` | `--page N` (required), `--session-dir <dir>` | never |
| `outcome state` | `--deliverable <path>` (required), `--session-dir <dir>` | never |
| `outcome verify` | `--session-dir <dir>` | a promise is unresolved, a delivered file is missing or empty, `fidelity_unresolved`, or `edition_unverified` |
| `outcome finish` | `--ledger <file>`, `--session-dir <dir>`, `--now <ISO>` | the fidelity barrier holds (section 8) |
| `outcome briefing` | `--json`, `--ledger <file>`, `--session-dir <dir>`, `--now <ISO>` | never |
| `format-extract <reference>` | `--out <design-spec.md>` (required), `--from-url` | never (an unsupported reference exits `2`) |
| `--help` | `--json` | never |
| `source snapshot` | `--id S<n>` (required), one of `--url <u>`, `--html <file>`, `--raw <file>` (optionally with `--text <extraction> --extractor NAME@VERSION`), `--text <file>` alone (a converted-only source: quotes only), `--command '<argv json>' --cwd <dir>`; `--charset <name>` (overrides the detected encoding), `--kind page\|command\|document\|converted-only`, `--provenance primary\|secondary` (default `unknown`, read by `source floor: primary only`), `--revision <r>`, `--replace` (overwrite the id's snapshot; refused while an active row cites it), `--session-dir <dir>`; the contract is in [fact-ledger.md](fact-ledger.md#snapshot-and-locator-contract) | the id already holds different content (`snapshot_exists`); `--replace` on an id an active row cites (`snapshot_pinned`) |
| `source refresh` | `S<n>` (required); no input flag re-fetches the source's http(s) URL, or one `source snapshot` input flag gives the new capture; `--charset`, `--session-dir <dir>` | a row's quote is not found once in the new snapshot (`quote_not_verbatim`; that row keeps its old pin) |
| `source probe-conversion` | `<raw-file> <converted-file>` (positional) | the converted text holds a number the raw text does not (`altered`, exit `1`) |
| `fact add` | `--source S<n>` with `--find <text>` (`--unit-expected <unit>`), or `--table <id> --row <label> --col <label>`, or `--page N --line N`; `--label`, `--entity`, `--valid-at`, `--population`, `--accounting-basis`, `--observation O<n>`, `--json`, `--session-dir <dir>`. Or `--assumed <value> --unit <unit> --rationale <text> --label <label>` | `fact_not_located`, `fact_ambiguous`, `unit_mismatch` (no row written) |
| `fact quote` | `--source S<n> --match <text>` (required), `--label`, `--json`, `--session-dir <dir>` | `fact_not_located`, `fact_ambiguous` |
| `fact retract <id>` | `--reason <text>` (required), `--replacement <id>`, `--session-dir <dir>` | never (an illegal transition exits `2`) |
| `fact export` | `--csv`, `--out <file>`, `--session-dir <dir>` | never |
| `fact fill <src>` | `--out <file>` or `--view`, `--format html\|md\|tex`, `--inline`, `--replace-failed`, `--brief <brief.md>`, `--session-dir <dir>` | `fidelity_interview_incomplete`, `derived_not_allowed`, `source_outside_floor` (section 4), or a fill error: no edition, fill map, or state is written; the ledger's cached `outside_floor` is (section 4). Off-table `derived numbers` or `fallback` exits `2` |
| `fact verify <report>` | `--format html\|md` (from the extension by default), `--sources <dir>`, `--claims <claims.json>`, `--fill-map <file>` (every edition needs one; by default `<report>.fill-map.json` or `<report>.polished.fill-map.json`, whichever records the edition's bytes, and none is `brief_changed`; the HTML edition's map also gives each value-mode fact's mode and the unit bound to it at fill), `--brief <brief.md>`, `--out <record>`, `--session-dir <dir>` | `fidelity_interview_incomplete` (no record written), or any blocker or major defect, `brief_changed`, `derived_not_allowed`, and `source_outside_floor` under the current brief included (section 4) |
| `fact pairs <report>` | `--format html\|md`, `--ledger <fact-ledger.json>`, `--claims <claims.json>`, `--changed-since <fill-map.json>` (the pre-polish fill map, or a polished map, which leads to it through `polishedFrom`: keep only the pairs whose sentence text is not in the artifact the fill map names, so the readers re-check exactly what changed; when that artifact no longer hashes to the map's `artifact_sha256`, because it was polished in place or edited, every pair is kept; the record's `changed_since` is `{fill_map, narrowed}`; a map without `artifact_sha256`, or a fill map changed after polish, exits `2`), `--brief <brief.md>`, `--out <pairs.json>`, `--session-dir <dir>` | `fidelity_interview_incomplete` (with `meaning check: off` it writes `{"skipped": true}` and exits `0`) |
| `fact polish-apply <report.html>` | `--edits <polish-edits.json>` and `--out <report.polished.html>` (both required; a missing flag or a Markdown edition exits `2`), `--fill-map <file>` (default: the edition's fill map, chosen as `fact verify` chooses it; one that does not record the input's bytes exits `2` and nothing is written), `--format html`, `--ledger <fact-ledger.json>`, `--brief <brief.md>`, `--session-dir <dir>` | `fidelity_interview_incomplete` (nothing written); otherwise never: it exits `0` whenever it wrote the output. With a fill map it also writes `<out>.polished.fill-map.json` (the brief binding, the polished file's hash, and `polishedFrom` naming the fill map) and never rewrites the fill map, even when `--out` is the input. Each rejected (`polish_edit_rejected`) or unlocated (`polish_edit_unlocated`) edit leaves its text unchanged and is printed verbatim, one line per rejection, in the summary and in `data.rejected_reasons`; `data` also carries `applied`, `rejected`, `unlocated`, `applied_indexes`, `input_sha256`, `edits_sha256`, and `out_sha256` |
| `fact polish-diff` | `--before <pre-polish.html>` and `--after <polished.html>` (both required; a missing flag exits `2`), `--ledger <fact-ledger.json>`, `--out <record>` (default `$SESSION_DIR/polish-defects.json`), `--brief <brief.md>`, `--session-dir <dir>` | `fidelity_interview_incomplete` (nothing written), or a registry blocker in the diff (majors and minors are reported and exit `0`; a defect code the registry does not know refuses with `unknown defect code <code>`). The record is `{command, status, exitCode, before, artifact, artifact_sha256, defects[], counts}`, with `counts` in contract buckets |
| `fact meaning-check` | `--pairs <pairs.json>` and `--verdicts <file>` (required; repeatable, one per reader tier), `--dispute P<n> --reason <text>`, `--run-id <id>`, `--calibration <file>` (default `fidelity-calibration.jsonl`), `--brief <brief.md>`, `--edition <format>` (writes `fidelity-meaning-defects.<format>.json`, the delivered edition's meaning record), `--out <record>` (default `fidelity-meaning-defects.json`), `--session-dir <dir>` | `fidelity_interview_incomplete`, or `meaning_drift_confirmed` (stderr carries one JSON line with the defect and dropped-verdict counts) |
| `fact derive` | `--op <op>` and `--now <ISO>` (both required; `--now` is the row's `observed_at`, the script never reads the clock, and without it the command exits `2` with `missing_now` and writes no row) with the operation's operands (section 11), `--unit`, `--to`, `--rationale`, `--disjoint`, `--label`, `--json`, `--session-dir <dir>` | `incompatible_operands` (no row written) |
| `claims index <claim-graph.md>` | `--out <claims.json>` (required) | never (a malformed claim table exits `2`) |

## 11. Fidelity gate

Every number, quotation, and chart value in a reader-facing edition comes from the fact ledger (`$SESSION_DIR/fact-ledger.json`, schema and operations in [fact-ledger.md](fact-ledger.md)), which is built only from source snapshots (`$SESSION_DIR/sources/S<n>/`). Nobody types a value into a report. The commands, in the order a run uses them:

```bash
node "$SKILL_DIR/scripts/report-tools.mjs" source snapshot --id S4 --url https://example.org/bridge --session-dir "$SESSION_DIR"
node "$SKILL_DIR/scripts/report-tools.mjs" fact add --source S4 --find "Span length" --label span_len --session-dir "$SESSION_DIR"
node "$SKILL_DIR/scripts/report-tools.mjs" fact quote --source S4 --match "no earlier than 2027" --label completion --session-dir "$SESSION_DIR"
node "$SKILL_DIR/scripts/report-tools.mjs" fact derive --op convert F1 --to mm --label span_mm --now "$(date -u +%Y-%m-%dT%H:%M:%SZ)" --session-dir "$SESSION_DIR"
node "$SKILL_DIR/scripts/report-tools.mjs" claims index "$SESSION_DIR/claim-graph.md" --out "$SESSION_DIR/claims.json"
node "$SKILL_DIR/scripts/report-tools.mjs" fact fill "$SESSION_DIR/report.src.html" --out "$SESSION_DIR/report.html" --session-dir "$SESSION_DIR"
node "$SKILL_DIR/scripts/report-tools.mjs" fact verify "$SESSION_DIR/report.html" --session-dir "$SESSION_DIR"
```

- **Snapshot.** A source directory is immutable: the same id with identical text is a no-op, different text fails with `snapshot_exists`, because ledger rows pin the old hash; use a new id, or `--replace` while no active row cites the id (the `conversion_altered_number` recovery). `--url` fetches with plain fetch (30 s timeout, no browser) and records extractor `tool:source-snapshot@1`. `--html` and `--text` import a capture the browser lane saved and record extractor `model` (lower trust), or the named extractor with `--text --extractor NAME@VERSION`. `--command` runs an argv without a shell, snapshots its stdout, and writes `command.json` with the argv, working directory, exit status, and output hashes.
- **Register.** `fact add` locates one quantity deterministically (`--find`: the first numeral within 80 characters after a unique anchor, in the same sentence; `--table`/`--row`/`--col` by header labels; `--page`/`--line`) and prints the row with 60 characters of source context on each side (`--json` for the full row). `fact quote` stores the whole containing sentence. The row records the source's kind, URL, host, and whether the brief's source floor points at it. A failed locate writes nothing.
- **Write by reference.** Placeholders: `{{F12:span_len}}` (value and unit), `{{F12:span_len:value}}`, `{{F12:span_len:unit}}`, `{{F12:span_len:q}}` (with the source qualifier), `{{Q7:completion}}`, `{{Q7:completion:lead=8}}`, `{{Q7:completion:from="no earlier":to="2027"}}`. The label must equal the row's label.
- **Fill.** `fact fill` is all or nothing: on `pointer_label_mismatch`, `unresolved_placeholder`, `quote_anchor_unresolved`, or `unit_duplicated` it exits `1` and writes nothing. On success it writes the output and `<out>.fill-map.json`. `--view` prints the rendered text without writing; `--inline` builds the inline edition (local stylesheets and scripts inlined, image paths made absolute) from the same source; `--format md` and `--format tex` fill Markdown and LaTeX sources; `--replace-failed` replaces a sentence that failed twice with an attributed block quote of its source.
- **Verify.** `fact verify` writes its record (default `fidelity-defects.json`; `--out` for another edition) with the verified file's `artifact_sha256`, reads `claims.json` and the locked `## Fidelity` block of `brief.md`, and exits `1` on any blocker or major defect; minors alone exit `0`. A Markdown edition (`--format md`, or a `.md` path) is verified with its fill map; a LaTeX edition is verified through its Markdown source, so `--format tex` exits `2`.
- **Meaning.** With `meaning check: on`, `fact pairs` writes the pairs a separate reader session judges, and `fact meaning-check --edition <format>` turns the readers' verdicts into that edition's `fidelity-meaning-defects.<format>.json` and the calibration log; run both once per delivered edition whose source bytes differ (the inline edition, a Markdown or LaTeX source) (verdict schema in [fact-ledger.md](fact-ledger.md)). No script calls a model.
- **Export.** `fact export --csv --out "$SESSION_DIR/assets/<chart>.csv"` writes `fact_id,label,plot_value,unit` for every active value row, the only data a chart may plot, and prints `N of M sources snapshotted, by extractor: ...` for the closing briefing.

Derivations are computed in exact rationals from the operands' stored values and rendered at the operands' fewest significant figures; a refused operation exits `1` with `incompatible_operands` and writes no row.

| Operation | Operands | Checks |
| --- | --- | --- |
| `sum` | two or more ids, `--unit` | one unit; summing `%` needs `--disjoint` and `--rationale` |
| `diff` | two ids (minuend, subtrahend) | one unit; a difference of `%` is in `pp` |
| `growth` | `--from <id> --to <id>` | one declared entity, two different `valid_at` periods, a positive start; result in `%` |
| `ratio` | `--num <id> --den <id>`, `--unit` | a non-zero denominator |
| `share` | `--part <id> --whole <id>` | one unit; result in `%` |
| `convert` | one id, `--to <unit>` | a known conversion between the units |
| `expr` | a formula over ids, `+ - * /`, `0`, `1`, `100`, `--unit`, `--rationale` | none (recorded `unchecked`) |

Exit codes: `0` ok; `1` a semantic failure (`fact_not_located`, `fact_ambiguous`, a fill error, `incompatible_operands`, `snapshot_exists`, `snapshot_pinned`, or a verify record with a blocker or major defect); `2` usage, an unreadable input, or an invalid ledger write.
