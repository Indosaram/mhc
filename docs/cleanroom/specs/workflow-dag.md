# Workflow runs (dependency graphs of delegated tasks) — behavior spec (Phase 4/5 gap)

Scope: a runner that executes a user- or model-authored graph of delegated tasks, where each node
is one child task (`task.md`) and edges say "start this node only after those nodes finished". It
is exposed as the `workflow` agent tool and the `/dag` command. The single-task runner, its model
choice, depth limit and state directory come from `task.md` and are reused unchanged; this spec
covers only what the graph layer adds. Interface facts reproduced verbatim: the tool name, its
parameter names and `action` values, the `/dag` command name, the `task.dag.*` config keys
(`config-keys.md`), status/state enum values and the error `code` strings.

Proposed neutral names: crate area `maho-task::workflow`, types `WorkflowDefinition`,
`WorkflowNode`, `WorkflowRun`, `WorkflowStore`, `WorkflowScheduler`.

## 1. Definition

A definition is `{key, name, nodes[]}`:

- `key` — idempotency key, unique per parent session (see §3).
- `name` — display name.
- each node: `id` (required), `prompt` (required), `label?`, `description?`, `task_summary?`,
  `dependsOn?` (array of node ids), `load_skills?` (array of skill names), and a target: exactly
  one of `category` or `subagent_type`; `model?` is accepted only together with `subagent_type`.

Edges carry no data: a node's prompt is dispatched exactly as stored (§3.2), whatever its
prerequisites produced. A node that needs earlier results reads them from the shared workspace or
from the persisted result files (§6).

### 1.1 Validation (all errors are reported together; any error rejects the whole definition)

| code | condition |
| --- | --- |
| `empty_graph` | no nodes |
| `node_count_exceeded` | more than `task.dag.max_nodes_per_run` nodes |
| `invalid_node_id` | id empty, `.` or `..`, or containing `/`, `\`, `:` or NUL (ids become file names) |
| `duplicate_node_id` | the same id twice (reported once per id) |
| `prompt_bytes_exceeded` | prompt UTF-8 length above `task.dag.max_prompt_bytes` |
| `dependency_fanout_exceeded` | one node lists more dependencies than `max_nodes_per_run` |
| `self_dependency` | a node depends on itself |
| `unknown_dependency` | a dependency names no node in the definition |
| `cycle` | a dependency cycle; the message lists the cycle path, first node repeated at the end |

Target errors (both/neither of `category`/`subagent_type`, `model` with `category`) are reported
per node before graph validation. The tool result for any of these has error code
`invalid_definition` and carries the per-node/per-rule details.

A valid definition may also produce advisory warnings (the set of warnings is up to the
implementer). Warnings never block the start; they are returned in the start result.

### 1.2 Derived graph data (shown in snapshots and `/dag`)

- **waves**: node ids grouped by depth — wave 1 holds nodes with no dependencies, wave *n* holds
  nodes whose deepest dependency is in wave *n-1*. Ids inside a wave are in a stable order.
- **edges**: `{from, to}` for every dependency.
- **critical path**: the longest dependency chain by node count; ties broken deterministically
  (lexicographic on ids).
- **bottlenecks**: per node, the number of transitive dependents, sorted by that count descending
  then by id.

## 2. States

Run status: `pending | running | paused | completed | failed | cancelled` (last three terminal).

Node state: `pending | blocked | scheduled | running | paused | completed | failed | cancelled |
skipped` (last four terminal). Each node also has `attempt` (starts at 1), an optional child task
id, start/finish timestamps, optional error `{code, message, at}`, and for settled nodes an output
preview (first 2000 characters) with the full byte length.

Node error codes: `plan_unresolved`, `depth_denied`, `start_failed`, `residency_denied`,
`task_error`, `task_interrupted`, `task_lost`, `task_cancelled`, `resume_task_missing`,
`resume_task_orphaned`, `lost_while_paused`, `journal_corrupt`.

## 3. Lifecycle and scheduling

1. **Start.** Validate, then look up `key` for this parent session. Same key and the same
   definition (compared by a canonical fingerprint) → the existing run is returned with
   `reused: true` and nothing is re-run. Same key with a different definition while the old run
   record exists → `definition_conflict`. A key whose run was already pruned by retention is
   treated as free. A new run gets a fresh run id; more than `task.dag.max_runs_per_session` runs
   for one parent session is an error.
2. **Skills.** For each node, the named skills' `SKILL.md` content is prepended to the prompt once,
   at creation, and the result is stored with the run. Later edits to skills never change an
   existing run (also not on resume). A missing skill is a diagnostic, not a failure.
3. **Frontier admission.** Every node whose dependencies are all `completed` is started as a child
   task (subject to the task runner's concurrency limits); unrelated branches run in parallel.
   Nodes waiting on dependencies are `blocked`/`pending`.
4. **Failure policy: continue independent work.** A failed or cancelled node does not stop
   unrelated branches. When nothing is running, every not-yet-started node that depends
   (directly or transitively) on a non-completed terminal node becomes `skipped`.
5. **Settle.** When no node can make progress the run becomes `failed` if any node failed (the run
   error is that of the first failed node in wave order), otherwise `completed`.
6. **Automatic infrastructure retry.** A node whose child ended because of an infrastructure
   problem (child unresponsive, host unavailable, session could not be resumed — never an ordinary
   child error, never a requested cancellation) is retried automatically up to 2 times with
   exponential backoff starting at 1 s. The retry count survives process restarts.
7. **Durability.** Every state change is appended to a per-run event journal and folded into a
   per-run record, written atomically. A run whose owning process died is recovered on the next
   session start for that parent session: children still running are re-attached, children that
   vanished mark the node `task_lost`/`resume_task_missing`.
8. **Retention.** Terminal runs older than `task.dag.retention_days` are deleted (record, journal,
   results, key) by a background sweep after session start; failures are logged, never fatal.

## 4. The `workflow` tool

Parameters: `action` (required) plus `definition`, `run_id`, `reason`, `node_id`, `node_ids`,
`message`, `prompt`, `detach`, `guidance_id`, `text`, `remove`. Every result has a short text line
and structured details whose `kind` is one of `started`, `attached`, `snapshot`, `detached`,
`waited`, `cancelled`, `retried`, `sent`, `amended` (one per action family) or `error`
(`{code, message, errors, diagnostics, node_ids}`).

| action | needs | behavior |
| --- | --- | --- |
| `start` | `definition` | §3.1; returns run id, `reused`, snapshot, warnings |
| `attach` | `run_id` | registers the calling session as the wake-up target of a run that is still live; returns snapshot |
| `snapshot` | `run_id` | current status, counts, nodes; names running nodes with no child activity for 10 minutes |
| `wait` | `run_id` | default `detach: true`: returns at once; afterwards a notification turn is triggered in the session for each node that settles and once more for the whole run; `detach: false` blocks until settle and returns per-node results; on a terminal run returns the result immediately |
| `cancel` | `run_id`, `reason?` | cancels all running children; run → `cancelled`, unstarted nodes → `cancelled`/`skipped` |
| `retry` | `run_id`, settled run | selected nodes go back to `pending` with `attempt`+1, in the same run. Selection: `node_id`, or `node_ids`, or (neither given) every node in `failed`/`cancelled`/`skipped`. Nodes in `completed` keep state and output. `prompt` is allowed only when exactly one node is selected |
| `send` | `run_id`, `node_id`, `message` | child mid-run: `message` is injected into its current run; child idle but resumable: `message` starts a follow-up run in that child's session; else `node_not_continuable` |
| `amend` | `run_id`, `definition` | `definition.key` must equal the run's key. Diff by node id: identical `completed` nodes stay as they are; new or modified nodes, plus every node downstream of them, go back to `pending`; nodes absent from the new definition are dropped. A diff touching a `scheduled`/`running` node → `amend_running_node` |
| `guide` | paused-by-user run | manages notes queued for delivery on user resume: add (`text`, optional `node_id`), edit (`guidance_id` + `text`), remove (`guidance_id` + `remove: true`) |
| `cancel_node` | paused-by-user run, `node_id` | the named node becomes `cancelled` and its child is cancelled; run status remains `paused` |
| `resume` | — | returns `run_paused_by_user` for a user-stopped run; such a run restarts only through the user-facing resume control |

Run ownership: a run is visible only to its parent session (root session recorded too); another
session gets `run_not_owned`.

Error codes (tool level): `invalid_definition`, `definition_conflict`, `run_not_found` (also for a
missing `run_id`), `run_not_owned`, `invalid_arguments`, `invalid_amendment`,
`amend_running_node`, `run_still_active` (retry/amend on an unsettled run), `node_not_found`,
`node_not_retryable`, `node_has_no_task`, `node_not_continuable`, `run_paused_by_user`,
`run_not_user_paused`, `guidance_frozen` (guidance already being delivered), `guidance_not_found`.

## 5. The `/dag` command

- `/dag` — up to 20 runs of the current session, newest first, one line each: name, run id,
  status, `<completed>/<total> done`, plus running/failed counts when non-zero; or a "no runs"
  notice.
- `/dag <run id>` — header (name, id, status, node count, wave count, amendment count if any),
  then per wave its nodes with label, state, target (`category:<name>` or `agent:<name>`), model,
  attempt when >1, duration, dependencies, a critical-path marker and the error code/message;
  then the critical path and bottlenecks. Unknown id → warning notice.
- All user-supplied text is sanitised for the terminal (control characters removed).

## 6. Storage and config

Under the task state directory (`task.md` §2), a `workflows/` subdirectory with: key index files
(per parent session + key hash), one record JSON per run, one JSONL event journal per run, one
result directory per run with `<node id>.txt` (full final answer of the node's child) plus a
stats sidecar, and lock files. Compatibility with stores written by the replaced code is not
required.

Config (`config-keys.md`): `task.dag.max_nodes_per_run` (64), `max_runs_per_session` (16),
`subscriber_ring` (1000, buffered events per live subscriber; overflow is reported, not fatal),
`heartbeat_ms` (15000, lease refresh of the owning process), `history_default_limit` (256) /
`history_max_limit` (1000) for event-history pages, `retention_days` (7), `max_prompt_bytes`
(262144). All are positive integers.

Platform notes: atomic writes via temp file + rename (retry a few times on Windows when the target
is briefly locked); process liveness for lease recovery via pid check on Unix, lease timestamps
only on Windows.

## 7. Acceptance tests

1. A three-node chain `a -> b -> c` starts only `a`; `b` starts after `a` completes; run ends
   `completed` with three waves.
2. Fan-out/fan-in: `a`, then `b1`,`b2`,`b3` depending on `a`, then `d` depending on all three:
   `b*` run concurrently, `d` starts only after the last `b*` completes.
3. Each validation rule in §1.1 yields its code; a definition with two problems reports both and
   creates no run.
4. A cycle `x -> y -> x` reports `cycle` with a path starting and ending at the same node.
5. A node with both `category` and `subagent_type`, or `model` with `category`, is rejected.
6. Starting twice with the same key and definition returns the same run id with `reused: true`
   and starts no new child; same key with a changed prompt returns `definition_conflict`.
7. Exceeding `max_runs_per_session` fails the start; a run pruned by retention frees its key.
8. In `a -> b` plus independent `c`, failing `a` lets `c` complete, marks `b` `skipped`, and the run
   ends `failed` with `a`'s error.
9. `retry` on the run from test 8 with no node ids re-runs `a` (attempt 2) and `b`, leaves `c`
   untouched; `retry` on a running run returns `run_still_active`.
10. `retry` with `prompt` and two node ids returns `invalid_arguments`.
11. An infrastructure failure is retried at most twice with growing delay; an ordinary child error
    is not retried automatically.
12. `amend` changing one completed node's prompt re-runs that node and its dependents only;
    amending a running node returns `amend_running_node`; a different key returns
    `invalid_amendment`.
13. `wait` with default `detach` returns immediately and later delivers one message per settled
    node and a final settle message; `detach: false` returns after settle.
14. `send` to a running node steers the child; to a node never started returns `node_has_no_task`.
15. `cancel` stops all children and the run reports `cancelled`.
16. A run from another session returns `run_not_owned`; an unknown id returns `run_not_found`.
17. `load_skills` content is prepended at start; editing the skill file afterwards does not change
    the stored prompt; an unknown skill yields a diagnostic and the run still starts.
18. Kill the owning process mid-run; a new session for the same parent re-attaches live children
    and finishes the run.
19. Retention: a terminal run older than `retention_days` is removed by the sweep; a running run
    is never removed.
20. `/dag` lists runs with counts; `/dag <id>` shows waves, critical path and bottlenecks;
    `/dag nope` gives a warning.
21. On a run stopped by the user: `resume` is refused with `run_paused_by_user`; `guide` add/edit/
    remove work; `cancel_node` cancels one node and the run stays `paused`; `guide` on a run that
    is not user-paused returns `run_not_user_paused`.

## Delta (omo v5.1.52, 2026-10-30)

Rows routed through `task.md` "Delta (omo v5.1.52)" D5; the full mapping table for the
`maho-task` lane is `task.md` D6. Message wording is the implementer's.

### D1. Nodes keep the parent session's model as their last candidate

Behaviour: when a run starts, the `workflow` tool reads the calling session's current model
(`provider/id`) and stores it in the run record as `parent_model` (optional string; absent when
the session has no model). Every dispatch of a node's child — first admission (§3.3), a manual
`retry`, an automatic infrastructure retry (§3.6), an `amend` re-run and recovery after the owner
died (§3.7) — passes that stored value as the parent model to the task model choice
(`task.md` §5). So a `category` node whose category models are all unavailable still falls back to
the model the run was started from, even when a different session (after restart) or no session
model is around at dispatch time.

- `parent_model` is fixed at start; a later model change in the session does not change it.
  `amend` keeps it. A reused run (`reused: true`) keeps the stored value.
- Explicit `model` on a `subagent_type` node still wins (§5 order is unchanged).
- A run record written before this field existed has no `parent_model`; dispatch then uses the
  current session's model if any (old behaviour).

Tests:
1. Session on `p/main`, category `c` with models `[x/unavailable]`, `available` = `{p/main}` →
   the node's child runs on `p/main`.
2. Start on `p/main`, then switch the session to `q/other`, then `retry` a failed node → the
   child again gets `p/main` as its parent model.
3. Recovery of a run whose owner died re-dispatches a pending node with the stored
   `parent_model`.
4. A `subagent_type` node with `model: m/x` runs on `m/x` regardless of `parent_model`.
5. Record JSON round-trip keeps `parent_model`; a record without it loads.

### D2. Skills from the validated bundled root

`load_skills` (§3.2) searches the bundled skills root resolved by the one shared resolver of
`skills.md` Delta D1/D2 (validated `MAHO_SKILLS_ROOT` override with its once-per-process notice),
not a separately computed path. An invalid override therefore produces the same single notice and
the same fallback root as the `skill` tool.

Test: with `MAHO_SKILLS_ROOT` set to a relative path, starting a run with `load_skills: ["s"]`
where `s` exists in the executable-sibling bundled root prepends that skill, and the invalid-
override notice appears at most once in the process.

### D3. Mapping

| census row | delta § | APPLIES/N-A | reason |
| --- | --- | --- | --- |
| omo-components-26 | D1 | APPLIES | tool sends the parent model on start |
| omo-senpi-task-b-15 | D1 | APPLIES | parent model persisted; used by admission, retry and recovery |
| omo-components-03 | D2 | APPLIES | node skills use the shared validated resolver |

Leak self-check: no SUL code or comment carried over; no constant tables; `parent_model` is a
neutral field name of this crate's own record; no OmO-original feature.
