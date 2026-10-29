# Keyed work pools — behavior spec (Phase 4, unit `workpool`)

Scope: a durable, keyed work queue owned by one parent session. The parent creates a pool bound to
one worker agent spec, pushes keyed JSON inputs, and later receives one aggregate of keyed results.
Workers are ordinary delegated tasks (`task.md`) that report per-key results back through a
`yield` operation. This spec builds on `task.md` (task records, run epochs, concurrency slots,
follow-up runs, cancellation) and `model-selection.md` §5 (worker model choice). Nothing here
defines new config keys.

Proposed neutral names: crate module `work_pool`, tool `workpool` (interface fact, kept because
existing transcripts and skills call it), types `PoolRecord`, `PoolItem`, `PoolWorker`.

## 1. Tool surface (interface facts)

One tool, `workpool`, with a required `op` field. Each op accepts exactly the fields listed;
any other field, a missing required field, or a blank string (empty after trimming) is rejected
with `invalid_input`. All strings below are trimmed before use.

| op | Fields | Caller |
| --- | --- | --- |
| `create` | `name`, `agent`, `mode?`, `tools?` | parent |
| `push` | `pool_id`, `items` | parent |
| `close` | `pool_id` | parent |
| `inspect` | `pool_id` | parent |
| `cancel` | `pool_id` | parent |
| `yield` | `results` | assigned worker only |

- `agent` is exactly one of `{category, prompt, model?}` or `{subagent_type, prompt, model?}`;
  both or neither of `category`/`subagent_type`, or extra fields, is `invalid_input`.
- `mode`: `fresh` or `keep_alive`; omitted means `keep_alive`.
- `tools`: list of extra tool names, provided by the parent's runtime, that workers of this pool
  may call (see §6). Omitted or empty means no extra tools.
- `pool_id`: `wp_` followed by 32 lowercase hex digits. A malformed id is `invalid_input` at the
  tool boundary (`invalid_pool_id` at the store boundary).
- `items`: list of `{key, input}`; `key` is a non-blank string, `input` any JSON value (including
  `null`); exactly these two fields.
- `results` (yield): list of `{key, data}` or `{key, error: {code, message}}`; exactly two
  fields per result, exactly two fields in `error`, all strings non-blank.

Responses are JSON objects (also returned as the tool's text content):

- `create` → `{pool_id, mode, generation, status}`.
- `push` → `{pool_id, item_ids: [{key, item_id}, ...]}` in request order. Item ids are
  `wi_` + 32 lowercase hex digits and never change.
- `close` and `cancel` → `{pool_id, status, generation}`.
- `inspect` → `{pool_id, name, mode, status, generation, revision, items, workers, aggregate}`.
  Each item: `key`, `input`, `item_id`, `status`, plus `data` (completed) or `error {code,message}`
  (error/cancelled). Each worker: `task_id`, `run_epoch`, `status` (`busy|idle|paused`),
  `completed_turns`, `idle_since` (ms). `aggregate`: `null` or `{generation, delivered, accepted}`.
- `yield` → per-result receipts in request order, each one of: accepted `{key, item_id}`,
  duplicate `{key, item_id}`, or refused `{key?, code, message}`.

Failures are tool errors whose text starts with a machine code from this set (interface fact):
`invalid_input`, `invalid_pool_id`, `pool_not_found`, `scope_denied`, `pool_name_conflict`,
`pool_closed`, `duplicate_key_conflict`, `tools_unavailable`, `admission_refused`,
`spawn_failed`, `policy_denied`, `worker_not_continuable`, `worker_unassigned`,
`stale_assignment`, `store_corrupt`, `cancelled`, `yield_conflict`, `item_missing_yield`,
`delivery_uncertain`, `tool_name_collision`, `kernel_tool_missing`. The same codes appear as
`error.code` on failed items.

## 2. Ownership and scope

- A pool belongs to the session that created it. `push/close/inspect/cancel` from any other
  session → `scope_denied`; an unknown id → `pool_not_found`.
- A session that is currently a worker of any pool may not call parent ops (`scope_denied`).
- The parent may not call `yield` (`scope_denied`). A session with no host identity → `scope_denied`.

## 3. Lifecycle

Pool status: `open` → `closing` (after `close`) or `cancelled` (after `cancel`). Item status:
`queued` → `assigned` → `completed` | `error`; `queued`/`assigned` → `cancelled`.

- **create**: the agent spec is checked against the spawn policy first (`policy_denied`, nothing
  stored), then resolved like a delegated task (`admission_refused` on failure), then extra tools
  are bound (§6). Create is idempotent per (parent session, name): an `open` pool with the same
  name, agent, mode, resolved worker spec and tool set (order and duplicates ignored) is returned
  unchanged; any difference, or a non-open pool with that name → `pool_name_conflict`. A new pool
  starts at `generation` 1, `revision` 0.
- **push**: only while `open` (else `pool_closed`). Push latency is bounded by the store write:
  the call returns the `item_ids` once they are persisted, independent of dispatch. A key already in
  the pool with canonically equal input (object key order ignored) returns its existing `item_id`;
  different input → `duplicate_key_conflict`, and then no item of that call is stored. An empty
  `items` list succeeds.
- **close**: `open` → `closing`; already closing/cancelled is a no-op. Remaining items still run.
- **cancel**: marks the pool `cancelled`, increments `generation`, turns every `queued`/`assigned`
  item into `cancelled` with error code `cancelled`, drops pending admissions, revokes extra
  tools, and cancels every worker task. Repeating cancel is a no-op. Cancel does not deliver an
  aggregate; its response is the final word.
- **revision** increases by one on every stored change; a no-op change does not bump it.

## 4. Dispatch and modes

- Items are dispatched in push order through the normal task concurrency limits (per-model slots,
  task depth, spawn policy). Waiting for a slot is not an error.
- `fresh`: every item starts a new worker task whose first prompt carries that item.
- `keep_alive`: an item goes to an idle worker of the pool when one exists, chosen by fewest
  `completed_turns`, then oldest `idle_since`, then task id. The item is sent as a follow-up run
  of that worker (a new run epoch). Otherwise a new worker is started if a slot is free; else the
  item waits until a worker goes idle or a slot frees.
- Every assignment message is a JSON object `{pool_id, generation, items: [{item_id, key, input}]}`
  plus an instruction to report each key with `yield` and not to delegate further.
- A worker is `busy` while its run is active and `idle` after the run ends (`completed_turns` +1).
- Admission failures (spawn failure, policy change, worker no longer resumable) mark the affected
  items `error` with the matching code; dispatch of other items continues.

## 5. Yield

- A worker may yield only keys bound to its current run (same task, run epoch and pool
  generation). No binding at all → call fails `worker_unassigned`; binding for an older epoch or
  generation, or a cancelled pool → `stale_assignment`.
- First valid yield for a key: appended durably to the worker task's event log, then the item
  becomes `completed` (`data`) or `error` (`error`). Receipt: accepted.
- Same key, identical result again → duplicate (no change). Different result → refused
  `yield_conflict`. Key not assigned to this run → refused `stale_assignment`. A malformed result
  entry → refused `invalid_input`; the other entries are still processed.
- When a worker run ends after its assignment was delivered, every assigned item without a yield
  becomes `error` `item_missing_yield`.
- If it cannot be established whether an assignment reached the worker (crash, delivery failure),
  the item becomes `error` `delivery_uncertain`, which is terminal for that item: the dispatcher
  never selects it again, and repeating the work requires a push under a new key. A later valid
  yield for such an item is still recorded in the worker's log and recovered on the next reconcile.

## 6. Extra worker tools

`tools` names tools from the parent's live runtime that workers may call. At create time each name
must exist (`kernel_tool_missing`), must not clash with a tool the worker already has
(`tool_name_collision`), and must pass the worker's allow/deny lists (`policy_denied`). If the
parent runtime that provides them is gone when a worker starts → that item fails
`tools_unavailable`. The `workpool` tool itself is never shared from parent to workers; workers
get a yield-only view.

## 7. Aggregate delivery

- When a pool is `closing`, every item is terminal, and the pool is not held (§8), the parent
  session receives exactly one aggregate per pool generation: a custom session message
  (`workpool-aggregate`) whose content is the JSON array of results in push order —
  `{key, data}` for completed items, `{key, error: {code, message}}` otherwise — and whose details
  are `{pool_id, generation, results}`. It is queued for the next idle point and triggers a turn.
- Closing an empty pool delivers an empty array.
- `aggregate` state is stored before delivery (`accepted: true`), set to `delivered: true` when the
  host confirms the message was written, and reset to `accepted: false` on a reported failure.
  A reported failure is surfaced as a pool event; the next delivery attempt happens only at the
  next flush point (a repeated `close` or the next session start), with no timer-driven attempts.
  On the next session start an accepted-but-unconfirmed aggregate is delivered again once. A
  confirmed aggregate is never sent twice.

## 8. Persistence, restart and pause

- One JSON record per pool under `<task state dir>/workpools/<pool_id>.json`, written atomically.
  An unreadable or invalid record → `store_corrupt` (never silently dropped).
- On session start the host reattaches every pool owned by that session: recovers yields from
  worker event logs, marks lost deliveries `delivery_uncertain`, marks finished workers idle,
  resumes dispatch, and flushes pending aggregates. Pools outlive the process that created them.
- When the user stops the parent run, its pools are held: busy workers become `paused`, nothing new
  is dispatched and no aggregate is delivered. Resume releases the hold (a resume for an older stop
  is ignored) and dispatch continues. Session shutdown stops dispatch without changing records.

## 9. Platform notes

Same as `task.md`: workers are child processes; file writes use temp-file + rename on all
platforms. Pool and item ids use lowercase hex only so they are safe as file names on Windows.

## 10. Acceptance tests

1. `create` with only `name` + `agent{category,prompt}` returns `mode: keep_alive`, `generation: 1`,
   `status: open`, and a `wp_` id of 35 characters.
2. `create` with both `category` and `subagent_type`, with a blank `name`, or with `pool_id` → `invalid_input`.
3. Repeating an identical `create` returns the same `pool_id`; changing `mode` → `pool_name_conflict`.
4. A denied spawn policy makes `create` fail `policy_denied` and leaves no record on disk.
5. `push` with key `" a "` stores key `a`; pushing `{"b":2,"a":1}` then `{"a":1,"b":2}` under
   the same key returns the same `item_id` and does not bump `revision`.
6. A push of `[new, a-with-different-input]` fails `duplicate_key_conflict` and `new` is not stored.
7. `push` after `close` → `pool_closed`; `push` with `items: []` on an open pool succeeds.
8. `inspect` from another session → `scope_denied`; on an unknown well-formed id → `pool_not_found`.
9. Parent calling `yield` → `scope_denied`; a worker calling `push` → `scope_denied`.
10. `fresh` pool with three items starts three distinct worker tasks.
11. `keep_alive` pool with concurrency 1 and three items uses one worker task over three run epochs;
    `completed_turns` ends at 3.
12. Idle-worker choice: workers (turns 1, idle 2), (turns 0, idle 9, id a), (turns 0, idle 9, id b)
    → `a` is chosen.
13. Worker yields `{key, data: 2}` → receipt accepted, item `completed`, `data` 2; same yield again →
    duplicate; `{key, data: 3}` → refused `yield_conflict`.
14. A yield entry with both `data` and `error`, or an `error` with an extra field → refused
    `invalid_input` while other entries in the same call are accepted.
15. Worker yields for a key from its previous run epoch → refused `stale_assignment`.
16. Worker run ends without yielding an acknowledged item → item `error` `item_missing_yield`.
17. `close` on an empty pool delivers one `workpool-aggregate` message with content `[]`.
18. Two items pushed `b`, `a`, both completed, then `close` → aggregate results in order `b`, `a`;
    a failed item appears as `{key, error{code,message}}`.
19. Aggregate is not delivered while any item is `queued` or `assigned`.
20. Acknowledged aggregate is not delivered again on the next session start; an accepted but
    unacknowledged one is delivered exactly once on the next start.
21. `cancel` with one busy worker: worker task cancelled, open items `cancelled` with code
    `cancelled`, `generation` incremented, repeated `cancel` changes nothing.
22. Restart: create, push, kill the host process, start a new host on the same state dir →
    `inspect` shows the same items and ids, and a yield already in the worker log is applied.
23. A corrupt pool record makes `inspect` fail `store_corrupt`.
24. Stop then resume the parent run: no dispatch or aggregate while held; resume with a stale stop
    sequence leaves the hold in place.
25. `tools: ["missing_tool"]` → `kernel_tool_missing`; a name the worker already has →
    `tool_name_collision`.
