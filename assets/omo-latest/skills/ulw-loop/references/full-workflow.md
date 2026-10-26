---
name: ulw-loop
description: Goal-like loop that uses ultrawork mode to decompose work into systematic, evidence-bound steps.
metadata:
  short-description: Goal-like ultrawork loop for systematic decomposition
---

## Role
Expert goal orchestration agent. You conduct; right-sized subagents play. Plan durable multi-goal work, fan independent work out, QA every result yourself, record only proven evidence.
Use GPT-5.x style: outcome-first, evidence-bound, atomic decisions, no nested branching prose.

## Goal
Deliver every goal in `.omo/ulw-loop/<session-id>/goals.json` end-to-end.
Prove EVERY success criterion with captured observable evidence from a real-usage scenario you ran (HTTP / tmux / browser / computer-use below).
TESTS ALONE NEVER PROVE DONE. A green test suite is supporting evidence, not completion proof.
Audit each pass, fail, block, steering change, and checkpoint in `.omo/ulw-loop/<session-id>/ledger.jsonl`.

## Manual-QA channels
Run each criterion's real-surface proof yourself through the channel that faithfully exercises it; capture the artifact before recording PASS.

1. **HTTP call** — hit the live endpoint with `curl -i` (or an HTTP client from js eval); capture status line + headers + body.
2. **Terminal / TUI** - prove it through the xterm.js web terminal; tmux `send-keys` is fine for a boot smoke, but NEVER `tmux capture-pane` for color/layout/CJK evidence (it degrades truecolor).
3. **Browser use** — omowright from js eval (staged in the `browser` skill): the owned engine (`connectPipe` on a task-owned profile, `connectCloakProfile` for bot-scored targets) for unauthenticated pages, the attached engine (`connectBrowserSkill()` in the user's signed-in browser) when the page needs their login — never a clone of, or a launch against, the live profile. Capture action log + screenshot path. Never downgrade a browser-facing criterion.
4. **Computer use** — for desktop/GUI apps, drive the running app via OS automation (computer-use, AppleScript, xdotool, etc.); capture action log + screenshot.

For TUI visual QA (mandatory when a PR or review must inspect the terminal screen),
render the live pty through a browser-hosted xterm.js terminal (the repository's
own web-terminal QA script when it ships one) and record the screenshot, a plain
transcript, and the run metadata.

Auxiliary surfaces (CLI stdout / DB state diff / parsed config dump) are first-class evidence for CLI- or data-shaped criteria; use a channel scenario when the behavior is user-facing. `--dry-run`, printing the command, "should respond", and "looks correct" never count.

## Delegation model (ATLAS-STYLE — YOU CONDUCT, WORKERS PLAY)
You read, search, plan, integrate, and QA. You DELEGATE every code edit, test write, bug fix, and QA execution to a right-sized `task` worker, then verify what comes back. Fan out independent tasks in PARALLEL in one response; serialize only on a NAMED dependency (one task consumes another's output or edits the same file). The skill's mhc tool mapping lists the tools.

Size each worker to the task: pick the `category` (or `subagent_type`) and put the intended role, rigor level, and specialty inside the worker `prompt`.

| Task shape | Prompt instruction and target |
|---|---|
| Trivial / mechanical (rename, move, obvious one-liner, config edit) | `TASK: act as a focused worker for a trivial mechanical edit. ...` + `category: "quick"` |
| Pure implementation against a clear spec (new function, endpoint, test from a named pattern) | `TASK: act as a high-rigor implementation worker. ...` + `category: "unspecified-high"` |
| Deep debugging / race / perf / subtle cross-module reasoning | `TASK: act as a deep debugging worker. ...` + `category: "deep-low"` (`deep-high` when the central decision cannot be settled from evidence) |
| QA execution (drive a channel, capture evidence) | `TASK: act as a QA execution worker. ...` + `category: "unspecified-high"` |
| Read-only codebase search | `TASK: act as an explorer. ...` + `subagent_type: "explore"` |
| Implementation — pick the tier by change SIZE: LOW small (one-file fix, boilerplate) / MEDIUM mid-sized (standard feature, a few files) / HIGH large (new module, cross-module, concurrency/security/migration, or a big complex problem with one clear goal) | `TASK: act as a <low|medium|high>-difficulty implementation worker. ...` + `category`: LOW `quick`, MEDIUM `unspecified-high`, HIGH `deep-high` (or `ultrabrain` for one hard cohesive problem) |
| External library / docs research | `TASK: act as a librarian. ...` + `subagent_type: "librarian"` |
| Final verification audit | `TASK: act as a rigorous final verification reviewer. ...` + `category: "deep-high"` |

For reviewer work, use a self-contained reviewer assignment, tight scope, and explicit verification in the `prompt`. Never spawn a context-only child for review.

Difficulty is orthogonal to LIGHT/HEAVY rigor: the category picks model power, the tier picks how much process.

Every worker message MUST carry: goal + exact files in scope; the PIN + failing-first proof before production code; constraints + project rules; verification commands; the ONE Manual-QA channel and exact artifact; for git-tracked edits, require `git-master` plus repo and touched-path commit history before commit. Workers have NO interview context — be exhaustive, and forward learnings.

Subagent reliability on mhc:
- Start every `task` prompt with `TASK: <imperative assignment>`, then name `DELIVERABLE`, `SCOPE`, `VERIFY`, and `STOP WHEN`. State that it is an executable assignment, not a context handoff. A child sees only its prompt (plus any `load_skills`), so paste exactly the context it needs.
- Plan and reviewer children may run for a long time; start them with `run_in_background: true` and keep doing independent root work. Their completion arrives as a notification; do not poll `task_output` in a loop.
- For work likely to run long, require the child to send `WORKING: <task> - <current phase>` before long reading, testing, or review passes, and `BLOCKED: <reason>` only when it cannot progress.
- While any child is active, keep the parent visibly alive with the active child count, their names, the latest `WORKING:` phase, and what the parent is waiting on.
- Track spawned task ids and names locally. A child's report is a claim, not proof. A quiet child is alive until `task_output` shows it terminal.
- Fallback only when the child is completed without the deliverable, ack-only, explicitly `BLOCKED:`, or no longer running. Then send `TASK STILL ACTIVE: return <deliverable> or BLOCKED: <reason>` with `task_send` when it can still recover the lane; otherwise record inconclusive, do not count it as pass/review approval, `task_cancel` it if it is still running, and spawn a smaller task with the missing deliverable.

## Artifacts
- `.omo/ulw-loop/<session-id>/brief.md`: original brief and durable constraints.
- `.omo/ulw-loop/<session-id>/goals.json`: goals with embedded `successCriteria` per goal.
- `.omo/ulw-loop/<session-id>/ledger.jsonl`: append-only audit trail.
- Read artifacts before resuming, steering, or checkpointing.
- After compaction or context loss, re-read brief + goals + ledger FIRST, then `mhc ulw-loop status --json`. Recover from artifacts; never re-plan from scratch or repeat completed work.
- Never invent state outside `.omo/ulw-loop` artifacts or `mhc ulw-loop status --json`.

## Bootstrap
Do all three steps before execution; no edits, goal tools or checkpointing until bootstrap completes. Follow the parent skill’s resource-rule pointer before execution.

### 1. Create goals from the brief
The CLI is the `mhc` binary itself: `mhc ulw-loop <command>` (`mhc ulw-loop help` lists every subcommand). When `mhc` is not on `PATH`, call the running session's executable by its absolute path; it is the same CLI, so PATH absence is not a blocker. If no `mhc` executable can be found at all, open the durable notepad, record the missing-CLI evidence in `.omo/ulw-loop/bootstrap-notepad.md`, and surface the installation issue instead of hand-writing state.

Run one form:
```sh
mhc ulw-loop create-goals --session-id <id> --brief "<brief>" [--validation-batch-json <json-or-path>] --json
mhc ulw-loop create-goals --session-id <id> --brief-file <path> [--validation-batch-json <json-or-path>] --json
cat <brief> | mhc ulw-loop create-goals --session-id <id> --from-stdin [--validation-batch-json <json-or-path>] --json
```
Every state subcommand runs against exactly one session scope: pass `--session-id <id>` on every call (the printed handoff and the resume directive carry this session's id; `PI_SESSION_ID`, which mhc's `bash` tool exports, or `OMO_ULW_LOOP_SESSION_ID` in the environment also resolve it). The CLI refuses with `ULW_LOOP_SESSION_SCOPE_REQUIRED` when neither is present instead of touching the shared `.omo/ulw-loop` root, because eval kernels, subprocesses, and hooks do not inherit the session env and every session in the directory would otherwise read and overwrite the same plan. Mutations are serialized across processes by `.omo/ulw-loop/<id>/.state.lock`, so parallel `record-evidence` calls from several workers are safe; `ULW_LOOP_LOCK_TIMEOUT` means another live process held the state for more than 10s — retry, never delete the lock while that process is alive.
If this session's aggregate is already complete, do not steer or force the
completed state for unrelated new work. Start a fresh run with
`mhc ulw-loop create-goals --session-id <new-id> ...` and keep passing
that id on every later call; automatic resume follows only the
session's own id, so a run under a custom id is resumed by hand. Use `--force`
only when deliberately overwriting completed evidence.
Write state through the CLI path. Do not hand-edit state files.

### 2. Refine success criteria + a Prometheus-grade QA and parallelism plan per goal
Shape every goal's objective and `successCriteria` by `references/define-goal.md`: its quality bar, objective anatomy, and criterion construction govern this step. Where the brief is silent on a constraint the work forks on, derive the default per that reference, record it via `annotate_ledger` (`--evidence` naming the repo fact, `--rationale` the default plus reversibility), and surface the assumed list in the first user-visible report so a wrong default is a one-line veto, not a finished run.
Gather context BEFORE planning with parallel `explore` / `librarian` workers plus your own read-only tools.
First survey available skills: read every loosely-relevant skill's description, deliberately choose which this work uses, and prefer applying genuinely-relevant skills over working raw.
Then run tier triage per goal — rigor (LIGHT/HEAVY below) and shape (`delivery` default, or `research` when the deliverable is a cited answer, not an artifact) — and record both in an `annotate_ledger` steering entry. Default is LIGHT — a narrow change inside existing layers. Take HEAVY only on a fact you can point to: a new module / abstraction / domain model; auth, security, or session; an external integration; a DB schema or migration; concurrency, transaction boundaries, or cache invalidation; a cross-domain refactor; or the user signaled care or demanded review. When unsure, take HEAVY; upgrade the moment a HEAVY fact surfaces, never downgrade mid-run.
Planning depends on unresolved design uncertainty, not the rigor tier: after discovery, delegate planning to a `task` child (`category: "ultrabrain"`, or `architect` for system design) only when unclear boundaries, competing decompositions, or uncertain dependency ordering remain; otherwise plan directly, including for HEAVY goals with a known procedure. HEAVY goals carry 3+ successCriteria covering happy path, edge, regression, and adversarial risk. LIGHT goals carry 1-2 successCriteria (happy path + the riskiest edge) with one real-surface proof of the deliverable.
Research-shape goals change the cycle: BEFORE each investigation, read this goal's prior ledger findings and open hypotheses, then extend them — never re-investigate an answered question (the ledger is your research notebook). Record findings via `annotate_ledger` with their source (`file:line`, command output, doc URL) as `--evidence`. Track hypotheses as `HYPOTHESIS[id]: <claim> | status: open`, flipped to `confirmed`/`refuted` only on an observed source. A research criterion passes on a cited answer — skip QA-channel, cleanup, and commit, but keep source-observability (never "looks correct"). Keep hypotheses inside the user's stated question; a scope-widening one is an `add_subgoal` proposal you surface, never silent creep. For a `research`-shape goal you MAY load `ulw-research` without hesitation — otherwise explicit-request-only, a research-shape goal IS that explicit demand. Research-only: never for a `delivery` goal. It composes with the librarian routing above — `ulw-research` for saturation (many parallel sources, recursive expansion), a single `librarian` for one lookup.
For each criterion, define upfront: `id`, exact `scenario` (tool + inputs + binary pass/fail), `expectedEvidence` artifact path, adversarial classes, stop condition, and Manual-QA channel. Vague QA ("verify it works") is a rejected criterion — revise it before execution. Every goal also declares, in one line, WHEN TO STOP: "stop right away when <the exact observable state that ends this goal>". A goal without that line is rejected — revise it before execution; the Stop Rules bind to it.
For optimization work, capture baseline speed before changes plus behavior/regression proof. Every attempt records speed, behavior/regression, and the keep/revert/iterate decision.
A criterion's adversarial classes are the ultraqa classes a fact about the change triggers: malformed input, prompt injection, cancel/resume, stale state, dirty worktree, hung or long commands, flaky tests, misleading success output, repeated interruptions. Record untriggered classes as not-applicable in one line.
Use channel-table evidence verbs — not vibes.

**Plan for maximum parallelism (HEAVY goals).** Decompose each goal's criteria into atomic tasks (Implementation + its Test = ONE task, never split) and group them into dependency waves. Target 5–8 tasks per wave; <3 per wave (except the final wave) means under-splitting — extract shared prerequisites into Wave 1. For each task record its wave, what it blocks, what blocks it, the worker tier from the Delegation table, and its QA scenario + evidence path. Build a dependency matrix (Task | Depends on | Blocks | Can parallelize with) and name the critical path. Anything not on a real dependency edge MUST share a wave and dispatch together.
Revise any criterion that lacks observable `expectedEvidence` or a named channel before execution.

### 3. Inspect state
Run `mhc ulw-loop status --json`.
Read pending goals, criteria IDs, current ledger head, blockers, and the aggregate `codexObjective` (the objective the session goal must carry).

## Execution Loop
Loop per goal. Cap at 5 cycles per goal. Cap identical same-criterion failures at 3.

### Acquire Next Goal
1. Run `mhc ulw-loop complete-goals --json` and read the handoff, including criteria. After the first goal starts, a successful complete checkpoint normally prints the next goal instruction directly; use `complete-goals` as the manual fallback/resume path.
2. Call `get_goal` and inspect the active session goal.
3. Apply this table exactly:

| get_goal result | action |
|-----------------|--------|
| no active goal | You MUST call `create_goal` — goal registration goes through the tool, never prose — with objective only from `instruction.json.objective`; do not copy lifecycle fields such as `status`. |
| same aggregate objective active | Continue the current ulw-loop story. |
| different goal active | STOP. Checkpoint blocked and surface the conflict. |
4. If retrying failed work, run `mhc ulw-loop complete-goals --retry-failed --json`.
5. Never create a second session goal for the same aggregate objective.

### Per-Criterion Cycle
1. PLAN: read `criterion.scenario`, `criterion.expectedEvidence`, prior ledger entries, and safety bounds. Identify which tasks in the current wave are independent.
2. Register atomic todos via `todo` — one ultra-granular step per action, `path: <action> for <criterion> - verify by <check>`. Call `todo` on every transition (`op: "start"` when a step begins, `op: "done"` when it finishes); exactly one `in_progress`, mark completed immediately, never batch, never let the rendered plan lag behind reality.
3. DELEGATE-IN-PARALLEL: dispatch every independent task in the wave at once via right-sized `task` workers (Delegation table). Each worker captures evidence failing-first: when the task touches EXISTING behavior, PIN it FIRST — a characterization test that asserts the current observable behavior and PASSES on the unchanged code, as rigorous as the new-behavior scenario (exact inputs, exact observable, exact assertion). Then RED through the cheapest faithful channel — a unit test where a seam exists, an integration/e2e test where the behavior lives in wiring, or the criterion's scenario captured failing when no test seam exists — failing for the RIGHT reason (no syntax/import error). A test that cannot fail for the regression it names (mock-call assertions, pinned constants, a fixture equal to the default it must override, an expected value re-derived from the output under test) is not evidence; use the scenario as the failing proof instead. TEST-ONLY tasks (regression coverage for behavior that is already correct) have no natural RED — require a mutation proof: temporarily force the exact regression each new assertion names, capture the assertion failing, revert the mutation, capture GREEN; an assertion that stays green under its mutation is not coverage. **When the target is PROSE (a prompt, `SKILL.md`, rule, or markdown/instruction file), the "observable behavior" is NOT the wording** — never pin sentences, phrase presence/absence, or word/char counts. PIN only a value a MACHINE consumes (a parsed frontmatter field, a sentinel token a hook greps, the doc's JSON sample run through its real validator), or guard two shipped copies with one `toBe` equality; a pure-prose change with no machine consumer has NO seam, so ship it on review + Manual-QA-by-read with NO automated test (a text grep there is pretend-coverage, not a RED proof). Then the SMALLEST GREEN change (none for a TEST-ONLY task — reverting the probe is GREEN; go to integration); before GREEN work that depends on external review, PR, issue, or branch state, refresh current branch/PR/issue state, preserve existing ordering/policy, and separate compatibility detection from policy changes unless the goal explicitly asks to change policy. A GREEN far larger than the criterion implies means the proof was too coarse — instruct a split. Serialize only on a NAMED dependency.
4. INTEGRATE + CRITICAL SELF-QA + GIT CHECKPOINT (EVERY WORKER RETURN): do NOT trust the worker's report. Read the diff yourself, re-run its tests, and run LSP diagnostics on the changed files. Treat "done" as a claim to disprove. If the diff drifts, the test is hollow, or evidence is missing, RESPAWN the worker with the specific failure context. Once the work unit is verified, use `git-master` before staging: inspect recent repository commits and touched-path history to infer commit language, Conventional Commit scope, message shape, and unit size. Stage only that unit's files and commit in the observed style; do not carry verified work forward into a later omnibus commit. If no git-tracked files changed or committing is unsafe, record the no-commit reason as evidence. Forward every finding/learning to subsequent workers.
5. EXECUTE-AS-SCENARIO: ACTUALLY run the Manual-QA scenario the criterion named (channel table above). Run it yourself for the orchestrator check; for heavier flows dispatch a dedicated QA execution worker (`category: "unspecified-high"` by default; `deep-high` when the QA flow itself is hard) whose ONLY job is to drive the channel and write the artifact to the named evidence path. If the scenario FAILS, respawn the implementing worker with the captured failure — do not hand-patch around it.
6. CAPTURE: collect the observable artifact path: transcript, stdout, screenshot, assertion, status+body, diff, or parsed dump. No artifact written at the evidence path — not done; record BLOCKED and respawn QA.
7. CLEAN (PAIRED, NEVER SKIP): execute the teardown todos before recording QA: owned PIDs (`kill`; verify `kill -0` fails), tmux sessions (`tmux kill-session -t ulw-qa-<criterion>`; confirm `tmux ls`), browser contexts (`.close()`), containers (`docker rm -f`), ports (`lsof -i :<port>` empty), QA scripts, temp paths and QA-only env vars. Finished workers end on their own; `task_cancel` any that are still running. Apply the ultrawork Task resources rule at task end, not after each criterion. Embed read-back receipts in the evidence and complete the cleanup todos. Missing receipt → record BLOCKED, not PASS.
8. RECORD one result immediately from the artifact you just wrote — never from memory or a later turn — stamping the capture tree `$(git rev-parse --short "HEAD^{tree}")` into the evidence:
   - PASS: `mhc ulw-loop record-evidence --goal-id <id> --criterion-id <id> --status pass --evidence "<observable> @tree:<short-tree> | <cleanup receipt>" --json`
   - FAIL: `mhc ulw-loop record-evidence --goal-id <id> --criterion-id <id> --status fail --evidence "<observable> @tree:<short-tree> | <cleanup receipt>" --notes "<diagnosis>" --json`
   - BLOCKED: `mhc ulw-loop record-evidence --goal-id <id> --criterion-id <id> --status blocked --evidence "<observable>" --notes "<safety/blocker/leftover-state>" --json`
9. If actual does not match expected, diagnose, respawn the right-sized worker with the failure context to fix minimally, and rerun the SAME criterion (including a fresh cleanup).
10. After 3 same-criterion failures, exit the goal with diagnosis.
11. After 5 cycles on one goal without required criteria passing, checkpoint failed.
12. Continue only when the next pending criterion has a concrete `expectedEvidence` target.

### Goal Completion
1. Non-final aggregate goal: confirm every `essential` criterion is `pass`; non-essential criteria may remain pending. Final aggregate goal: confirm every criterion across the whole plan is `pass`.
2. Call `get_goal` for a fresh snapshot.
3. Run `mhc ulw-loop checkpoint --goal-id <id> --status complete --evidence "<criteria evidence summary>" --codex-goal-json <snapshot> --json`; on success it auto-starts and prints the next eligible goal unless `--no-advance` is passed.
4. If blocked or failed, checkpoint with `--status blocked` or `--status failed` and include diagnosis evidence.
5. If this is the final goal, run the final quality gate first and pass `--quality-gate-json`.

## Exact final-story sequence
For the final story, follow this exact checkpoint sequence:

```sh
mhc ulw-loop status --json
# Read nextActions and currentAttemptDir.
mhc ulw-loop record-evidence --goal-id <g> --criterion-id <c> --status pass --evidence "..."
# Repeat record-evidence once per criterion.
# Then use the harness update_goal tool with status complete.
mhc ulw-loop checkpoint --goal-id <g> --print-template --json
# Fill the printed template: replace every placeholder and use real artifact paths under currentAttemptDir.
# codex-goal-json.goal.objective must equal the plan's codexObjective verbatim.
mhc ulw-loop checkpoint --goal-id <g> --status complete --evidence "..." --codex-goal-json <path> --quality-gate-json <path>
mhc ulw-loop complete-goals
```

The gate requires `manualQa`, `gateReview`, `iteration`, and `criteriaCoverage`; `codeReview` is optional (leave it out unless a code-review lane ran — the CLI rejects it on the `omo-senpi` surface). On mhc, record self-review as `"by": "main-session"` and a delegated gate review as `"by": "category:<name>"` with the category the reviewer ran under (`category:deep-high`, `category:deep-low`, `category:unspecified-high`, ...); `category:*` is accepted for `gateReview` on every surface, `main-session` for `manualQa` on every surface. Spawn reviewer lanes only when strict review is explicitly requested.

## Final Quality Gate
Trigger only for the final aggregate goal after every criterion in every goal is `pass`.
1. Run targeted verification for changed behavior.
2. FREEZE first — no more edits or rebases. At the frozen HEAD, re-run Manual-QA for any PASS criterion whose stamped tree differs from `git rev-parse --short "HEAD^{tree}"`, so every criterion is proven on the frozen tree; each artifact exists and is non-empty.
3a. Spawn a code-review child and a hands-on QA child in parallel (`task` with `run_in_background: true`, e.g. `category: "unspecified-high"` for each) with brief, goals, desired outcome, diff, evidence, and the report path each must write; wait for BOTH completions and confirm their report artifacts exist on disk.
3b. Only then spawn the gate reviewer (`task` with `category: "deep-high"`) with those artifact paths.
3c. The gate's approval binds to the frozen tree and full commit SHA and covers its three lanes — code quality, hands-on QA, and goal verification. Immediately append one durable `.omo/ulw-loop/ledger.jsonl` record per passing lane with the lane name, full SHA, verdict, and report artifact/source. Before reuse after continuation or compaction, re-read the ledger and require the exact lane/SHA pair; memory or an unstamped report is not coverage. A later rebase or amend that keeps the tree identical still has a new SHA and needs fresh lane stamps; changed content needs fresh review of the delta.
4. Treat timeout, missing deliverable, ack-only, `BLOCKED:`, or inconclusive review as a blocker. Any fix restarts the freeze at the new HEAD: re-run ONLY the proofs it invalidated and stamp the fresh output — never regenerate all evidence or relabel stale output to HEAD — re-review the delta at most TWICE; then record-review-blockers (step 5) and surface to the user.
5. If review remains blocked, run `mhc ulw-loop record-review-blockers --goal-id <id> --title "<...>" --objective "<...>" --evidence "<review findings>" --codex-goal-json <snapshot> --json`.
6. If clean, checkpoint final completion:
```sh
mhc ulw-loop checkpoint --goal-id <id> --status complete --evidence "<e2e evidence + manual QA notes>" --codex-goal-json <snapshot> --quality-gate-json <json-or-path> --json
```
`--quality-gate-json` shape. In `manualQa.artifactRefs`, `kind` must be one of `cli-transcript`, `log`, `screenshot`, `image`, `http-dump`, or `data-diff`; review and QA reports belong in `codeReview.reportPath` or `gateReview.reportPath`, not `artifactRefs`. `surfaceEvidence.surface` must be one of `cli`, `http`, `tmux`, `browser`, `gui`, or `data`. Compatibility is `cli`/`tmux` -> `cli-transcript`/`log`, `http` -> `http-dump`, `browser`/`gui` -> `screenshot`/`image`, and `data` -> `data-diff`.

`--quality-gate-json` shape:
```json
{
  "codeReview":{"by":"main-session","recommendation":"APPROVE","codeQualityStatus":"CLEAR","reportPath":"test/fixtures/artifacts/code-review.md","evidence":"Diff review passed.","blockers":[]},
  "manualQa":{"by":"main-session","status":"passed","evidence":"CLI and data surfaces passed.","surfaceEvidence":[{"id":"surface-cli-pass","criterionRef":"C1","surface":"cli","invocation":"mhc ulw-loop checkpoint --quality-gate-json sample-quality-gate.json --json","verdict":"passed","artifactRefs":["artifact-cli-pass"]},{"id":"surface-data-pass","criterionRef":"C2","surface":"data","invocation":"diff -u before-ledger.json after-ledger.json","verdict":"passed","artifactRefs":["artifact-data-diff"]}],"adversarialCases":[{"id":"adv-malformed-input","criterionRef":"C3","scenario":"malformed gate input omits manual QA evidence","expectedBehavior":"validator rejects ULW_LOOP_QUALITY_GATE_INVALID","verdict":"passed","artifactRefs":["artifact-cli-reject"]}],"artifactRefs":[{"id":"artifact-cli-pass","kind":"cli-transcript","description":"CLI pass artifact.","path":"test/fixtures/artifacts/cli-pass.txt"},{"id":"artifact-cli-reject","kind":"log","description":"Reject log artifact.","path":"test/fixtures/artifacts/rejection.txt"},{"id":"artifact-data-diff","kind":"data-diff","description":"Data diff artifact.","path":"test/fixtures/artifacts/data-diff.txt"}]},
  "gateReview":{"by":"category:deep-high","recommendation":"APPROVE","reportPath":"test/fixtures/artifacts/gate-review.md","evidence":"Gate review passed.","blockers":[],"notes":[]},
  "iteration":{"fullRerun":true,"status":"passed","rerunCommands":["cargo test -p <crate> <test-name>"],"evidence":"Focused rerun passed."},
  "criteriaCoverage":{"totalCriteria":3,"passCount":3,"originalIntent":"User wanted artifact-backed completion.","desiredOutcome":"Behavior ships with review and QA evidence.","userOutcomeReview":"Result matches brief and goals.","adversarialClassesCovered":["malformed_input","stale_state"]}
}
```
Artifacts must be non-empty; counts alone fail. LIGHT without adversarial class records `"adversarialClassesCovered": ["none-applicable: <reason>"]`; untriggered adversarialCases may use verdict `not_applicable` + `reason`; WATCH passes, notes surfaced.

## Dynamic Steering
Use steering only for structured evidence-backed mutation. Reject natural-language steering requests.

| Kind | When to use | Required fields |
|------|-------------|-----------------|
| add_subgoal | Real blocker found; new story required | `--title`, `--objective`, `--evidence`, `--rationale` |
| split_subgoal | Story too large; needs decomposition | `--goal-id`, `--children` JSON, `--evidence`, `--rationale` |
| reorder_pending | Discovered dependency order | `--order` JSON array of ids, `--evidence`, `--rationale` |
| revise_pending_wording | Title/objective ambiguous | `--goal-id`, `--title?`, `--objective?`, `--evidence`, `--rationale` |
| revise_criterion | Criterion lacks observable PASS evidence | `--goal-id`, `--criterion-id`, `--scenario?`, `--expected-evidence?`, `--evidence`, `--rationale` |
| annotate_ledger | Audit-only note | `--evidence`, `--rationale` |
| mark_blocked_superseded | Old story replaced by new evidence | `--goal-id`, `--replacements?`, `--evidence`, `--rationale` |

Command form: `mhc ulw-loop steer --kind <kind> [<kind-specific-fields>] --evidence "<...>" --rationale "<...>" --json`. For multiple evidence-backed plan-shape changes discovered together, pass `--proposals-json <json-or-path>` with an array of proposals; the batch applies atomically or rejects without partial plan mutation.

Validation batches are optional aggregate-mode review boundaries declared at create time with `--validation-batch-json`. A batch-final member requires all other members resolved, all member criteria pass, and a member-spanning quality gate; split/supersede steering keeps batch membership updated.
Structured prompt directives accepted: `OMO_ULW_LOOP_STEER: { ... }`, `omo.ulw-loop.steer: {...}`, `omo ulw-loop steer: {...}`, `ulw-loop steer: {...}`.

## Constraints
1. NEVER call `update_goal` mid-aggregate; only on final story after the quality gate passes.
2. NEVER call `create_goal` when `get_goal` shows a different active goal.
3. Evidence is bound to the tree it was captured at; changed tracked content invalidates it — re-run the QA at the current HEAD and re-record (an identical tree after rebase/amend stays valid). NEVER mark PASS from memory, and NEVER relabel, pin, refresh, or regenerate prior output to a moved HEAD.
4. NEVER bypass the criteria gate: non-final aggregate completion requires all essential criteria; final aggregate completion requires all criteria across the whole plan.
5. Baseline build/lint/typecheck/test commands are necessary evidence, NOT SUFFICIENT completion proof. Criteria coverage with observable evidence is the gate.
6. Treat `.omo/ulw-loop/ledger.jsonl` as the durable audit trail; checkpoint after every success or failure.
7. Per-story goal mode is opt-in only with `--codex-goal-mode per_story`; default is aggregate.
8. Structured steering directives mutate state through validation; normal prose does not.
9. Evidence MUST be observable from the real surface per the Manual-QA channel table — never a printed command, `--dry-run`, or "looks correct".
10. Probe the adversarial classes each criterion's trigger facts name (list in Bootstrap step 2); record untriggered classes as not-applicable in one line.
11. After completing an aggregate ulw-loop run, clear the session goal manually with `/goal clear` before starting another in the same session.
12. The shell command emits a model-facing handoff; only the agent itself calls the `get_goal`, `create_goal`, or `update_goal` tools.
13. NEVER record PASS while any QA-spawned process, `tmux` session, browser context, bound port, container, temp path, or open worker is still alive; the evidence MUST carry the cleanup receipt. Leftover state = BLOCKED.
14. DELEGATE all code edits, test writes, fixes, and QA execution to right-sized `task` workers (Delegation table); you read, search, plan, integrate, and QA. NEVER record `--status pass` from a worker's self-report — only from evidence you re-verified yourself. Dispatch independent tasks in parallel; serialize only on a NAMED dependency.
15. Every verified work unit that touched git-tracked files must leave either an atomic `git-master`-style commit hash or explicit no-commit blocker evidence before the next unit starts.

## Stop Rules
- STOP GOAL: all goals complete plus every plan criterion `pass` plus final quality gate clean. The decisive test — outranking every other consideration — is whether the completion conditions are FUNDAMENTALLY fulfilled and the user's problem ACTUALLY SOLVED in observable behavior; a `pass` ledger never substitutes for it. The moment both hold, checkpoint, report, and STOP — no extra review cycles, no evidence regeneration, no polish.
- 3x same criterion failure: checkpoint failed, surface diagnosis.
- 5 cycles on one goal without required criteria passing: checkpoint failed, surface.
- Safety boundary such as destructive command, secret exfiltration, or production write: block and surface a safe substitute.
- `get_goal` reports a different active goal: checkpoint blocker, stop, surface.
- Leftover state from QA (live process, `tmux` session, browser context, bound port, temp dir): NOT pass. Clean up, append the receipt, then continue.
- User issues `/cancel`: release in-progress state cleanly and do not auto-resume.
