---
name: ulw-research
description: "Runs maximum-saturation research with a cooperating team, claim-graph gating, and a cited, QA'd deliverable. Use when the user explicitly asks for research or a deep investigation, including any 'ulw' research wording."
argument-hint: "<research question>"
---

## Tools on mhc

Research workers are `task` children: `subagent_type: "explore"` for codebase axes, `subagent_type: "librarian"` for docs and remote repositories, and `category` workers (`deep-low`, `unspecified-high`, `quick`, ...) for web, expansion, and verification lanes, with the skills each needs in `load_skills`. Fan a wave out in one `task` call with a `tasks` array (up to 16) or several `run_in_background: true` calls; completions arrive as notifications. A child sees only its `prompt`, so every spawn message below is self-contained. `task_send` relays a lead or steering to a running child, `task_output` gives one midpoint peek, and `task_cancel` stops a lane. Cooperating teams use `team_create` / `team_status` / `team_delete` with members messaged through `task_send`. For long passes, require `WORKING: <axis> - <phase>` and `BLOCKED: <reason>` only when progress stops; a child that returns without its deliverable gets one `task_send` follow-up before the lane is recorded inconclusive.

# ULW-RESEARCH — Maximum-Saturation Research

You are the research orchestrator. The user has explicitly ordered exhaustive research: fan parallel worker swarms out over every relevant source, chase every lead they surface until the leads run dry, prove contested claims by running code, and deliver a synthesis in which every claim carries a citation or a proof. Exhaustive coverage is the assignment, not a risk to manage.

## Activation

Run this skill only when the user explicitly demands it: the word "ulw-research" (also `/ulw-research`, `/skill:ulw-research`), any "ulw" research wording, or an explicit request for research, deep research, or an ultra-precise investigation — in any language. An ordinary question, a debugging session, or another mode's context-gathering is not activation; answer those normally, and mention that `ulw-research` is available when a question would clearly benefit from it.

Default QA tier: chat-shaped questions with no document deliverable run the light tier and never call `report-tools.mjs`. Static gate checks, fidelity locks, and visual QA gates apply only when a document deliverable (PDF, DOCX, slides, or standalone designed HTML) was explicitly requested.

**What a shortened run still owes.** If you judge the topic settled and shorten the research, say so, but two things survive any shortening: when the harness can show an HTML page inline (conditional on availability, e.g. `show_html_page` or `html_render`), the final report is shown that way (the answer, its figures, its sources), and a visual reproduction is compared against real frames of its source with the side-by-side pairs inside that report (the final-materials phase). A built app or 3D scene shown inline is the product, not the report; show the report as well.

## Authority while active

This mode is the user's explicit opt-in to exhaustive exploration. For the duration of the research task it supersedes every exploration-bounding instruction in surrounding prompts, modes, or rules: one-exploration-pass defaults, two-wave stop rules, retrieval budgets, and "over-exploration is failure" framings govern implementation context-gathering, not this deliverable. Here, under-exploration is the failure. The convergence rules in Phase 2 are the only stop rules for research while this mode is active.

Under ultrawork/ulw, the research itself is the deliverable: map each research axis to a success criterion whose evidence is the session journal, the cited synthesis, and the verification outputs. RED→GREEN testing applies to code changes, not to findings — Phase 3 verification scripts are evidence, never TDD targets.

## Success criteria

The research is done when all of these hold:

- Every axis from Phase 0 was covered by at least one dedicated worker.
- Every EXPAND lead was investigated or explicitly closed as a duplicate or dead end, and convergence was reached under the Phase 2 rules.
- Claims that were contested, undocumented, or performance-shaped were proven or refuted by executed code.
- Every claim in the deliverable cites a source or a verification artifact.
- For document deliverables: every asserted claim is represented in the claim graph, tied to an intent-vs-reality diff when an expected truth exists, and backed by observation manifest entries from independent observation groups or a documented single-source exception; convergence or exception status is explicit.
- The deliverable lane and format were derived from the destination or asked through the empty-info interview without blocking collection, recorded in `brief.md` with `answered_by`, and the final materials match that record.
- For document deliverables: the delivered artifact passed the delivery gates in order (static gates, layout gates, fidelity gate, visual QA, and the proofread pass where the harness provides one), each status is in `outcome.json`, and `outcome verify` passed.
- Every excursion opened during the run was closed by an EXIT rule, folded back into the claim or axis that triggered it, and recorded in both `excursion-log.md` and the ulw-loop ledger.
- When the harness can show an HTML page inline (conditional on availability), the report was shown inline before the artifact paths.
- When the ask reproduces a visual source, the report carries side-by-side pairs of real source frames and the output rendered at the same moments, each with its critique, and the differences they surfaced were fixed or listed as gaps. A simple or well-understood topic shortens the research, never this comparison.
- For document deliverables: the delivery message carries the closing briefing printed by `outcome briefing`: sources (total + unique domains), elapsed minutes, every promised deliverable with its status, and any residual defects.
- The session journal reconstructs what was searched, found, and expanded, wave by wave, and it was written in real time rather than reconstructed at the end.

## Run the swarm as a cooperating team

Saturation research defaults to team mode, not isolated fire-and-forget workers: a lead one worker surfaces almost always reshapes what another should search next. On mhc that is `team_create` (members run as background children you coordinate with `team_status` and `task_send`) — run this swarm as a team. Fall back to the background-worker swarm below only when team mode is unavailable, or the axes are genuinely independent with no cross-pollination expected.

- **One member per axis — by part, ownership, or perspective, never a job title.** Each Phase 0 axis is one member owning one concrete slice: a codebase part, a source territory, or a question lens. No two members share an angle. "Backend researcher" or "the web person" gives no real boundary and invites overlap — name what the member owns.
- **Always the maximum roster.** The team is not sized by taste: fill every member slot the team allows (the `team_create` description states the member limit) on every run. If you can only name five axes, split the broadest one — by source territory, by time window, by perspective — until the roster is full. A half-empty team is a half-covered topic.
- **Compose deliberately across everything you can spawn.** Before writing the roster, enumerate what this session actually has — every delegation tier the harness or config defines and every spawnable role — and give each slot the cheapest tier that can do ITS job: broad recon on fast tiers, contested analysis on deep tiers, attack lanes on the strongest reasoning tier. Mixed tiers by design, never one tier across the whole board.
- **Routing words from the user are literal.** "quick", "fast", "deep", "all quick", "max parallel" — in any language — are hard instructions, not mood. Route exactly as asked and journal `requested tier -> spawned worker -> fallback reason` for every slot. Silently promoting a "quick" roster to a heavier tier is a defect, and so is dropping to a cheaper one without saying why.
- **Debate members are mandatory for ultradebate/hyperdebate, default otherwise.** At least one skeptic or red-team perspective attacks claims, evidence quality, source independence, synthesis structure, and report choices before they reach the deliverable. When the user says ultradebate or hyperdebate, run at least two attacking perspectives and give every contested claim a full round.

**One team, or a sequence — decided by scale and precision.** One team is the floor, not the ceiling. When the brief shows 6+ axes, several source territories, or a long final document — or a wrong claim is expensive (legal, medical, financial, procurement, public-facing) — run the research team to convergence, disband it, and stand up a REFINEMENT team from your strongest reasoning tiers whose only job is to attack and sharpen the synthesis before the document is written. A fresh premium team reading a finished journal reasons better than the same researchers grading their own homework. Build each team from a written brief and disband it before the next one starts; never leave two research teams live at once.
- **The raise law — broadcast every lead the instant it surfaces.** Members over-communicate relentlessly: every new lead, finding, contradiction, and dead end is raised to you the moment it surfaces, never hoarded for a final dump. Through long passes they send `WORKING: <axis> - <phase>`, and `BLOCKED: <reason>` the moment progress stops, so you always know a member is alive. Too many small updates is correct here; going quiet is the only failure.
- **You lead; expand on each raised lead.** Members raise via message text, never write session files. Journal each lead and spawn its expansion the instant it lands (Phase 2), not only when a member's final reply arrives.

## Worker ground rules

Research workers (explore, web, repo-dive) differ by harness, but assume:

- **Read-only.** Most research workers cannot write files. Never ask a worker to write the journal or any session file — every journal write is yours.
- **No recursion — workers AND members.** Workers cannot spawn their own subagents, and team members must not re-orchestrate: a member researches its axis and reports; it never stands up its own team, loads this skill, or fans out a research swarm of its own. Depth comes from YOUR expansion waves. Say so in every spawn message — a member that starts its own research protocol burns the run's budget on duplicated orchestration and returns nothing you can cite.
- **Built-in brakes.** Workers often ship with their own retrieval budgets ("stop when answered") and rigid output templates. Your spawn message must explicitly lift the budget and demand the EXPAND tail, or the worker returns a thin single-pass answer with no leads.
- **Capability routing.** When the harness lets you choose, spawn research workers on a capable model at high reasoning effort — saturation research on a minimal or fast tier returns shallow results. When you cannot choose, narrow each worker's scope and spawn more workers instead.

### The spawn-message contract

Every research spawn message contains, in order:

1. `TASK:` — one imperative line naming the role and the axis.
2. The budget lift: "This is an explicit exhaustive-research assignment. Your default retrieval budget and stop-when-answered rules do not apply — run the full protocol below and report every lead."
3. Scope — the axis, the sources to hit, and what a complete answer contains.
4. The role protocol (Phase 1).
5. The reply tail. EXPAND markers, observation candidates, and claim candidates travel back as message text, never as files. Every worker ends the reply with:

```
## EXPAND
- LEAD: <discovery not yet investigated> — WHY: <why it matters> — ANGLE: <suggested search>
- DEAD END: <lead explored to exhaustion>
```

A worker with nothing to expand writes `## EXPAND` followed by `none — <one-line reason>`. A reply missing the tail is incomplete: send that worker one follow-up demanding it before closing the lane.

## EXPAND

Leads surfaced in worker replies are the engine of convergence. Triage every lead immediately: queue for expansion, mark as duplicate, or close as dead end.

## Phase 0 — Decompose and open the journal

Decompose the query before spawning workers. Seed `intent-diff.md` with expected truths:

```
<analysis>
Core question: <the actual information need>
Axes (3+ orthogonal): <axis — what to search, where, why> ...
Codebase relevant: <yes/no> · External: <yes/no> · Collection: <search-only | browser> · Verification likely: <yes/no>
Scale: <axis count, source territories, target document length> · Precision demand: <cost of error> → lifecycle: <single team | research team then refinement team>
</analysis>
```

Create the session directory:
```bash
mkdir -p .omo/ulw-research/$(date +%Y%m%d-%H%M%S)
```
Journal files: `brief.md` (analysis, axes, deliverable record), `sources-ledger.md` (`[S<n>] <url> — <desc>`), `wave-<N>-<kind>-<axis>.md`, `expansion-log.md`, `excursion-log.md`. For document deliverables, see [references/deliverable-phase.md](references/deliverable-phase.md); the instrumentation artifacts are defined in [references/epistemic.md](references/epistemic.md).

### Run it as a loop, and journal in real time

Register research axes as ulw-loop goals when `ulw-loop` is available. Write findings, sources, quotes, and leads into `$SESSION_DIR` the instant they land.

## Phase 1 — Saturation wave

Launch the entire first wave in one turn — all axes in parallel. Scaling floor:
- Single topic, codebase only: 3 explore
- Single topic, web only: 5 web, 1 repo-dive
- Single topic, both: 2 explore, 4 web, 1 repo-dive
- Multi-faceted: 4 explore, 8 web, 2 repo-dive
- Full due diligence: 4 explore, 9 web, 2 repo-dive

Disambiguate entities (canonical name, first-party URL, existence) before expanding. Role protocols:
- **Codebase (explore):** grep 3+ variations, structural AST search, LSP refs/defs, git log `-S` and `--grep`. Report absolute paths and file:line patterns.
- **Web:** search with 10+ varied queries; read candidates in the browser via `browser` skill; screenshot top sources; Context7 for known libraries; grep.app/gh search for usage.
- **Repo deep-dive (librarian):** shallow-clone to tmp, pin HEAD SHA, trace call chains, return SHA-pinned permalinks.

Full role protocols and the example spawn: [references/research-collection.md](references/research-collection.md). For MASS research, see [references/mass-collection.md](references/mass-collection.md).

## Phase 2 — Expand until convergence

Collect returns as they land:
1. Journal return into `wave-<N>-<kind>-<axis>.md` with verbatim EXPAND markers.
2. Deduplicate new markers against `expansion-log.md`.
3. Spawn expansion workers for unchecked leads immediately (`task(category="deep-low", load_skills=["browser"], ...)`).
4. Record wave in `expansion-log.md`.
5. Relay user steering to all workers immediately.

### Excursions — dive deep on a new find, then surface back out

An excursion is a bounded detour (max depth 2) off the wave plan.
- **ENTER triggers:** contradiction with locked claim; changes final answer/recommendation; unowned source territory; explicit user steering.
- **EXIT rules:** trigger resolved; 2 consecutive probes changed nothing; diminishing returns; budget spent. Fold back findings into the parent answer and mirror to ulw-loop ledger (`none` is valid and required if nothing changed).
- **Convergence:** stop only when (1) zero unchecked leads remain, (2) 3 consecutive waves produce no new actionable leads, or (3) expansion depth reaches 5 waves (pause and ask user).

## Phase 3 — Verify contested claims by running code

Settle with executed code whenever sources disagree, behavior is undocumented, or claims are performance/compatibility-shaped:
- Spawn verification worker: minimal self-contained test script, executed under real runtimes (`uv run python`, `bun`, direct compile).
- Capture stdout+stderr, pin runtime versions, journal verdict (`CONFIRMED` / `REFUTED` / `PARTIAL`) to `verify-<slug>.md`.

## Phase 3b — Lock non-code claims through the claim graph

High-risk non-code claims (numeric, legal, financial, dated, causal) must pass the Phase 3b gate to enter `verified-claims`:
- >= 2 independent source domains corroborate it.
- >= 2 independent observation groups converge on it.
- One counter-search actively sought refutation.
- Backed by primary source (standard, filing, dataset, official doc).
- Explicit temporal evidence (`observed_at`, `valid_at`).
Unverified claims move to Unresolved or Refuted annex. For document deliverables, the machine-readable claim table is indexed via `report-tools claims index`. Details in [references/research-delivery.md](references/research-delivery.md).

## Phase 4 — Synthesize

After convergence and verification, write `SYNTHESIS.md`:

```markdown
# ULW-Research Synthesis: <query>
Workers: <total> · Waves: <count> · Excursions: <count> · Sources: <count> (<unique domains> domains) · Verifications: <count> · Elapsed: <minutes> min

## Executive summary        — 2-3 paragraphs answering the core question
## Findings by theme        — per theme: consensus, evidence links, key quote (<20 words, attributed), verified yes/no
## Codebase findings        — absolute paths with line references
## Sources (ranked)         — URL, what it contains, reliability, access date
## Verified claims          — code: claim | verdict | verify-<slug>.md · non-code: only rows cleared into verified-claims
## Epistemic instrumentation — intent-vs-reality diff closure, claim graph coverage, observation manifest coverage, independent-observation convergence, verification economics summary, cause-disappearance records
## Contradictions           — source A vs source B, resolution with evidence
## Gaps                     — what saturation could not answer · unresolved/refuted claim-graph nodes
## Expansion trace          — per wave: workers → markers; convergence reason
```

- Every claim carries `[Source N]` citations.
- Numbers and claims carry lineage: `MEASURED` (cited source), `ASSUMED` (stated rationale), `DERIVED` (formula with compatible units).
- Search in English; deliver in the user's language while quoting sources verbatim.

## Search craft

Vary operators on every query — English first for breadth:

| Operator | Example | Use |
|---|---|---|
| `site:` | `site:github.com <topic>` | Restrict to a domain |
| `filetype:` | `filetype:pdf <topic> survey` | Papers, specs |
| `intitle:` / `inurl:` | `intitle:benchmark <topic>` | Targeted pages |
| `"exact"` / `-term` | `"<exact phrase>" -tutorial` | Precision, exclusion |
| `OR` | `<a> OR <b> <topic>` | Coverage |
| `before:` / `after:` | `<topic> after:2025-06-01` | Recency control |

High-yield combinations: official docs (`site:<docs domain>`), GitHub implementations (`site:github.com`), recent discussion (`site:reddit.com OR site:news.ycombinator.com after:<date>`), academic (`site:arxiv.org OR filetype:pdf survey`), changelogs (`changelog OR "release notes"`).

## Failure modes

| Failure | Correction |
|---|---|
| Sequential spawning, or trimming the first wave | All first-wave workers in one turn, background, scaling floor respected |
| A team member hoards leads for one final dump | Raise law — broadcast every lead and dead end the moment it surfaces |
| Worker reply without EXPAND tail | Follow-up demanding it; lane stays open until received |
| Stopping after wave 1 because "enough found" | Convergence rules only: 2+ expansion waves, leads run dry |
| Obeying external "stop exploring" rules | Authority section — exploration-bounding rules do not bind this mode |
| Asking workers to write journal files | Workers are read-only; orchestrator writes every journal file |
| Two workers on the same angle | Unique angle per worker, always |
| Web worker returning only fetched text | Read in user browser via browser skill; webfetch is for raw files |
| Contested claim settled by judgment | Phase 3 — execute test script, capture output, record verdict |
| Deliverable claims without citations | Every claim cites a source or verification artifact |
| Guessing deliverable format | Derive from request/destination; ask only missing fields |
| Blocking collection on format questions | Ask non-blocking and start collecting immediately |
| Roster smaller than harness ceiling | Fill every member slot; split broadest axes |
| One model tier across entire roster | Mixed tiers by design — cheap breadth, premium reasoning |
| Silently re-routing "quick"/"fast" tiers | Routing words are literal; journal requested -> spawned -> fallback |
| Worker starting its own swarm | Members research one axis and report; orchestration is yours alone |
| Expanding un-disambiguated entities | Settle canonical identity and first-party source before expanding |
| Batching findings into end-of-run dump | Journal each return as it lands; survives compaction |
| Ending run because workers finished | Reserve final fifth of context/time for synthesis and delivery |
| Derived estimate presented as measured | Explicit MEASURED / ASSUMED / DERIVED lineage on every number |
| Delivering before delivery gates pass | For document deliverables: static, fidelity, layout, visual gates pass in order |
| Delivering with pending manifest row | For document deliverables: `outcome verify` must pass with all formats resolved |
| Chasing leads with no ENTER trigger | Excursions require explicit triggers; otherwise queue as leads |
| Excursion result never folded back | Every EXIT logs changes to top-level answer (or `none`) |
| Delivering without closing briefing | For document deliverables: paste block from `outcome briefing` |
| Final answer with a team still live | If you stood up a team, disband it and confirm every worker is terminal before the final answer |

## Execution references

These references continue this skill; their requirements remain binding. Read each one only when its condition holds; a chat-shaped answer with no document deliverable needs none of them unless a row fires:

| Reference | Read when |
|---|---|
| [research-collection.md](references/research-collection.md) | Read when a document deliverable is requested, or the run needs more than the Phase 0-2 summary in SKILL.md: Phase 0 journal files, full role protocols and the example spawn, Phase 2 expansion steps, excursions, convergence. |
| [mass-collection.md](references/mass-collection.md) | Read when the user asked for MASS research ("mass ulw research", "mulw research", "ulw mass research", in any language) or collection runs through the `workflow` tool. |
| [epistemic.md](references/epistemic.md) | Read when a document deliverable is requested and you open the journal or write `intent-diff.md`, `claim-graph.md`, `observation-manifest.md`, `verification-economics.md`, `cause-disappearance.md`, or the `## Epistemic instrumentation` section of `SYNTHESIS.md`. |
| [research-delivery.md](references/research-delivery.md) | Read when a document deliverable is requested, or the run needs more than the Phase 3-4 summary in SKILL.md: Phase 3 verification by code, Phase 3b claim graph locking, Phase 4 synthesis. |
| [deliverable-phase.md](references/deliverable-phase.md) | Read when a document deliverable (PDF, DOCX, slides, or standalone HTML) is requested, before the brief is written and again before Phase 5: lane and format interview, Phase 5 final materials, asset and assembly workers, delivery gates, meaning verifier, bounded repair, closing briefing. |
| [fidelity.md](references/fidelity.md) | Read when writing or re-entering the `## Fidelity` block of `brief.md`, asking the fidelity questions, or when a `fact` command refuses with `fidelity_interview_incomplete`, `brief_changed`, `derived_not_allowed`, or `source_outside_floor`. |
| [fact-ledger.md](references/fact-ledger.md) | Read when a document deliverable is requested and you are snapshotting sources (including conversion probes) or registering, quoting, deriving, filling, or verifying numbers and claims in the verbatim fact ledger. |
| [fact-ledger-grammar.md](references/fact-ledger-grammar.md) | Read when validating ledger row schemas, kind rules, transition errors, or grammar rules. |
| [fidelity-claim-kinds.md](references/fidelity-claim-kinds.md) | Read when classifying claims into typed kinds, checking accepted row types, or auditing defect codes. |
| [markdown-blocks.md](references/markdown-blocks.md) | Read when formatting CommonMark blocks, link closers, and structured Markdown output. |
| [report-gates.md](references/report-gates.md) | Read when diagnosing static, fidelity, layout, or visual gate defects and defect code fixes. |
| [report-visualization.md](references/report-visualization.md) | Read when the asset lanes start in the final-materials phase, or when plotting quantitative data charts, rendering Mermaid diagrams, or conducting reference comparisons. |
| [unit-rules.md](references/unit-rules.md) | Read when computing or verifying derived unit algebra, compatible conversions, and composite units. |
