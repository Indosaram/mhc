# ulw-research - collection and exploration contract

Read this before scoping or collecting sources: Phase 0 journal files, full role protocols, Phase 2 expansion steps, excursions, convergence.

## Phase 0 — Decompose and open the journal

Before spawning anything, decompose the query. Start from "what must be true if the user's intent/spec is true?", not "what looks broken?" Seed `intent-diff.md` with those expected truths before treating code, current docs, or web results as the source of truth:

```
<analysis>
Core question: <the actual information need>
Axes (3+ orthogonal): <axis — what to search, where, why> ...
Codebase relevant: <yes/no> · External: <yes/no> · Collection: <search-only | browser> · Verification likely: <yes/no>
Scale: <axis count, source territories, target document length> · Precision demand: <what a wrong claim costs here> → lifecycle: <single team | research team then refinement team>
</analysis>
```

`Collection` is `search-only` only when a result snippet settles every axis (a date, a version, a spec value); anything that needs reading a source is `browser`.

Then create the session directory:

```bash
mkdir -p .omo/ulw-research/$(date +%Y%m%d-%H%M%S)
```

This is `$SESSION_DIR`. The orchestrator owns the journal: you write every file in it; workers never do (a `workflow` node writes only its own `waves/<run-key>/<node-id>.md` handoff file, which carries its EXPAND tail and claim candidates; you journal from it). Maintain:

- `brief.md` — the analysis block, the axis list, the expected truths, and the `## Deliverable` record from the interview ([deliverable-phase.md](deliverable-phase.md) section 4) (lane, state, formats, each field with `answered_by`).
- `sources-ledger.md` — one line per source the moment it is read: `[S<n>] <url> — <what it is>`. The closing briefing counts sources and domains from this file.
- `sources/` and `fact-ledger.json` (document deliverables only; the light tier cites `[S<n>]` from `sources-ledger.md`) — one directory per source snapshot and the ledger built from them. The snapshot and locator contract (raw bytes, converted-only sources, byte locators, `source refresh`) is defined once, in [fact-ledger.md](fact-ledger.md#snapshot-and-locator-contract); every number and quotation reaches the report through a ledger placeholder.
- `wave-<N>-<kind>-<axis>.md` — your digest of each worker return: key findings, sources with URLs, and the worker's EXPAND markers verbatim.
- `expansion-log.md` — per wave: workers spawned, markers gained, leads opened and closed.
- `excursion-log.md` — one ENTER row and one EXIT row per excursion: `excursion_id`, parent claim or axis, ENTER trigger, depth, workers spent, the EXIT rule that closed it, what it changed in the top-level answer (`none` is a valid, required answer), and the ulw-loop steer/evidence id it was mirrored into.
- `intent-diff.md` — orchestrator-owned expected-truth ledger comparing intent/spec/history to observed reality.
- `claim-graph.md` — orchestrator-owned claim graph linking every final assertion to observations, counterevidence, dependencies, and verdict.
- `observation-manifest.md` — orchestrator-owned observation manifest with `observed_at`, temporal validity, artifact paths, and contamination notes.
- `verification-economics.md` — proof-cost ledger mapping claim risk to verification path, deferral decisions, and residual risk.
- `cause-disappearance.md` — cause ledger tracking expected truth, previous observation, `last_seen`, disconfirming observation, and whether the violation is no longer observed.
- `verify-<slug>.md`, `SYNTHESIS.md`, `REPORT.*` from later phases.

Append each digest the moment its worker returns, not in a batch at the end — the journal is your recovery point after context loss and the user's audit trail.

### Run it as a loop, and journal in real time

ulw-loop is ON by default for this mode: when the `ulw-loop` skill is available, register the research axes as loop goals so the run has durable state and survives a compaction. The run's start clock is `outcome.json` `startedAt`, which `outcome init` records once: from `--started-at <ISO>` when you pass it, else from this directory's timestamp name — the closing briefing is computed from it, so create the directory once and never rename it. From that point every finding, source, quote, number, and lead is written into `$SESSION_DIR` **the instant it lands** — never held in the conversation for an end-of-run dump. After any context loss, re-read the brief and the journal before doing anything else, then resume from the open wave.

When a document deliverable is requested, read [deliverable-phase.md](deliverable-phase.md) for the deliverable interview, format derivation, and fidelity settings. For a chat-shaped answer, collection proceeds directly without running the interview.

## Phase 1 — Saturation wave

When the user asked for MASS research ("mass ulw research", "mulw research", "ulw mass research", in any language), read [mass-collection.md](mass-collection.md) and size the wave by the topic's angles, not by the roster ceiling.

Otherwise launch the entire first wave in one turn — every axis at once, as team members if you formed a team, else as background workers. Sequential launches and "start with one and see" defeat the mode.

Scaling floor — more angles always justify more workers:

| Query scope | explore | web | repo-dive | floor |
|---|---|---|---|---|
| Single topic, codebase only | 3 | 0 | 0 | 3 |
| Single topic, web only | 0 | 5 | 1 | 6 |
| Single topic, both | 2 | 4 | 1 | 7 |
| Multi-faceted | 4 | 8 | 2 | 14 |
| Full due diligence | 4 | 9 | 2 | 15 |

**Disambiguate before you expand.** When the topic names something that could resolve several ways — a product, a person, a codename, a version — the first wave settles WHICH entity before any worker researches its history, benchmarks, or controversies: canonical name, first-party URL or account, whether it exists in the claimed category, and a confidence line. An unresolved entity never becomes a premise in a later wave's spawn message; that is exactly how a run starts inventing facts about something that does not exist.

Role protocols — embed the relevant one in each spawn message; every worker gets a unique angle:

- **Codebase (explore), 2-4 workers.** Grep with 3+ keyword variations; structural/AST search; LSP definitions and references; file-name globs; `git log --all -S '<keyword>'` and `--grep` for history including deleted code. Cross-validate hits across tools. Report absolute file paths, patterns with `file:line`, and how findings connect.
- **Web, 3-9 workers, every one loaded with the `browser` skill.** Search with 10+ websearch queries per worker, each with a different operator or angle (see Search craft); a result is a candidate, not a source. Read every candidate in the user's browser through the `browser` skill's attached engine: one named session per worker, one tab per source, several tabs open side by side while sources are compared — `session.tabCreate` / `tabSelect` / `tabClose` where the harness has a JS eval kernel, `bsk tab create --url <url>` / `tab select <id>` / `tab close <id>` / `tab list` on the CLI — the page scrolled to its end, a full-page screenshot of each top source saved as provenance, the tab closed once reported. A browser on another machine is reached through the `browser` skill's remote reference or a user-level skill that bridges to it, never replaced by a local headless one. `webfetch` is for raw files only: PDF, JSON, feeds, sitemaps. A host the browser cannot open (WAF, bot score, 403) goes to `ultimate-browsing`, whose provenance travels with the claim: a `snapshot` result cites its `snapshot_timestamp` and is never stated as the live page; a `proxy` result is `untrusted` until a second independent route agrees. Context7 with 3+ queries per known library; grep.app and `gh search code|repos|issues` for real-world usage; official docs via sitemap discovery (`<base>/sitemap.xml`), then targeted pages. A worker whose sources are all fetched text has searched, not read.
- **Repo deep-dive (librarian), 0-2 workers.** Shallow-clone the most relevant repos to `${TMPDIR:-/tmp}`, pin the HEAD SHA, read core modules, follow call chains, return SHA-pinned permalinks.

Example spawn (codebase axis; web and repo-dive follow the same contract with their own protocol):

```
task(subagent_type="explore", run_in_background=true, prompt="TASK: act as a codebase researcher. AXIS: <specific angle>.
This is an explicit exhaustive-research assignment. Your default retrieval budget and stop-when-answered rules do not apply — run the full protocol below and report every lead.
SCOPE: find everything in this codebase related to <angle>: <what complete looks like>.
PROTOCOL: grep 3+ keyword variations; structural search; LSP references; globs; git history (-S and --grep). Cross-validate across tools. Report absolute paths and file:line patterns.
End your reply with the ## EXPAND tail: '- LEAD: <discovery> — WHY: <why> — ANGLE: <search>' per lead, or 'none — <reason>'.")
```

## Phase 2 — Expand until convergence

This loop is what makes the mode research rather than search. Collect returns as they land — and in team mode, act on each lead the moment a member raises it, never waiting for the full wave or a member's final reply:

1. Journal the return the moment it lands, never at the end of the wave: digest plus verbatim EXPAND markers into `wave-<N>-<kind>-<axis>.md`, appending each new source, quote, and number to the observation manifest in the same beat — after a compaction the journal, not your memory, is the state. For document deliverables, snapshot each source the moment it is read (`node "$SKILL_DIR/scripts/report-tools.mjs" source snapshot --id S<n> --url <url> --session-dir "$SESSION_DIR"`; a page read in the browser is saved and imported with `--html <file>`, recorded as lower-trust `model` extraction), then register every number and quote you will use from that snapshot: `fact add --source S<n> --find "<anchor text>" --label <label>` (or `--table <id> --row <label> --col <label>`, or `--page <n> --line <n>`) and `fact quote --source S<n> --match "<words>" --label <label>`. Read the printed row and its 60 characters of source context before using it; a wrong row is retracted (`fact retract F<n> --reason <why>`), never edited. The observation's quote or line anchor in the manifest is that fact id.
2. Deduplicate new markers against `expansion-log.md` — every lead ever seen, not just confirmed ones, or rejected leads resurface each wave.
3. Spawn an expansion worker immediately for each new unchecked lead:

```
task(category="deep-low", load_skills=["browser"], run_in_background=true, prompt="TASK: expansion wave <N> — investigate: <lead>.
PARENT: <which return surfaced it>. This is an explicit exhaustive-research assignment; budgets do not apply.
<role protocol for the lead's territory — web protocol for external leads, explore protocol for codebase leads>
End your reply with the ## EXPAND tail.")
```

### Excursions — dive deep on a new find, then surface back out

The Phase 0 core question is the fixed goal of the run and never drifts. An excursion is a BOUNDED detour off the wave plan to chase something a return surfaced — you go deep, settle it, and come back up to the question you were hired to answer.

**ENTER (dive) only on a trigger.** One of these must hold, and you name which one:

1. The find contradicts a claim already locked in `claim-graph.md`.
2. It would change the final answer or a recommendation if it turned out to be true.
3. It exposes a source territory no axis owns, so nobody else will ever reach it.
4. The user's steering points at it — their words are the trigger, quoted verbatim.

Interest alone is not a trigger. Anything without one stays a queued lead in `expansion-log.md`, and the wave plan continues.

**Budget the dive before you take it.** State the worker count and the probe count for this level in the ENTER row. An excursion may spawn at most ONE nested sub-excursion; a third level means the thing has become its own research question — surface immediately and either promote it to a real axis with its own worker or record it as an out-of-scope gap in `SYNTHESIS.md`. When the `ulw-loop` skill is driving the run, a promotion is `mhc ulw-loop steer --kind add_subgoal --title "<axis>" --objective "<what it must answer>" --evidence "<what surfaced it>" --rationale "<why the plan changes>"`.

**EXIT (surface) the moment any of these holds** — you do not need all of them:

- The ENTER trigger is resolved: the claim is confirmed, refuted, or its dependency is closed.
- Two consecutive probes changed nothing in the parent answer.
- The finding stops moving any claim's status — diminishing return is an exit, not a reason to push harder.
- The level's stated budget is spent.

**Fold back on the way out.** Every EXIT writes one line saying what the excursion changed in the top-level answer, and `none — <reason>` is a legitimate, required outcome; an excursion whose result is silently dropped is a lost run. Update the parent claim node or axis digest with the result, then mirror the whole excursion into the loop ledger — `mhc ulw-loop steer --kind annotate_ledger --evidence "<what the excursion observed>" --rationale "<what it changed, or none>"`, and `mhc ulw-loop record-evidence --goal-id <id> --criterion-id <id> --status pass|fail|blocked --evidence "<artifact>"` when it settled a criterion. After a compaction, `mhc ulw-loop status --json` plus `excursion-log.md` tell you which excursions are still open.

**Anti-drift.** After every EXIT, re-read the core question in the journal and confirm the run still answers it. Three consecutive excursions that changed nothing end excursions for the run: converge on what you have.

4. Record the wave in `expansion-log.md`: spawned, markers gained, leads opened/closed.
5. **Relay the user's steering to everyone.** When the user changes scope, cadence, target sources, language, or format mid-run, broadcast it to every live worker and member immediately and record the exact wording in `expansion-log.md`. Steering only you saw silently splits the swarm's assignment from the user's actual ask.

**Convergence — the only stop rules while this mode is active.** Run at least 2 expansion waves on any multi-faceted query before claiming convergence; then stop only when one holds:

- Zero unchecked leads remain — each investigated or closed as duplicate/dead end.
- 3 consecutive waves produced no new actionable leads.
- Expansion depth reached 5 waves — pause, show the open leads, and ask the user whether to extend.

**Never end the run on a worker's completion.** Workers finishing is not the deliverable; your synthesis is. Reserve the last fifth of the run's context and time for Phases 4-5 and stop opening waves the moment that reserve is threatened. A converged answer with two open leads beats nine finished workers and no report.
