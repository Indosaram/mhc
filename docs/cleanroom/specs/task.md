# Delegated tasks — behavior spec (Phase 2, step 2)

Scope: the parts of `crates/maho-task` that have **no MIT original** to port. The task model
itself (agent definitions, ids, concurrency, result envelope, background delivery, follow-up runs)
is ported from MIT oh-my-pi (`parity.d/maho-task.md`). This spec was written only from public
documentation, the MIT oh-my-pi sources, the mhc CLI help (`crates/maho-cli/src/cli/help.txt`),
`docs/cleanroom/specs/config-keys.md` (`task`, `agents`, `categories`) and
`docs/cleanroom/specs/model-selection.md` §5. No OmO/SUL source was read.

## 1. Why a process runner

oh-my-pi runs subagents as in-process sessions. mhc's MIT extension API exposes no host call that
creates a nested agent session, so a child runs as a separate non-interactive mhc process. This
keeps the child's tools, extensions (LSP, rules, ...), provider logins and configuration identical
to a normal mhc run, and lets a background child outlive the parent process.

## 2. State directory

- Root: `task.state_dir` when set (relative to the session cwd), else
  `<agent dir>/tasks/<parent session id>` (session id characters outside `[A-Za-z0-9_-]` become `_`).
- Per task `<id>`: `<id>.json` (record), `<id>.md` (final answer of the last run),
  `<id>.log` (stderr of the last run), `<id>.prompt.md` (agent prompt + category `prompt_append`).
- `sessions/`: the children's session files (`--session-dir`).
- Record fields: id, agent, description (first line of the task, ≤120 chars), status
  (`queued|running|completed|failed|cancelled|interrupted`), model, thinking, model_source,
  session_id (UUID v4, fixed for the life of the task), cwd, background, pid, exit_code, error,
  runs, started/finished timestamps (ms), changed_files, hook_notes, tools, exclude_tools, isolated.
- Writes are atomic (temp file + rename).
- An active record (`queued`/`running`) not owned by a run in this process, whose pid is absent or
  dead, is reported as `interrupted` (its owner process ended). It can be resumed.

## 3. Child command

`<current exe> -p --mode text --session-dir <root>/sessions --session-id <session id> --name <id>
[--model <provider/id>] [--thinking <level>] [--append-system-prompt <root>/<id>.prompt.md]
[--tools a,b] [--exclude-tools x,y] -- <prompt>`

- cwd = the task cwd (or the isolated workspace's cwd); stdin = null; stdout → `<id>.md`;
  stderr → `<id>.log`; environment inherited plus `MHC_TASK_DEPTH=<parent depth + 1>` and
  `MHC_TASK_ID=<id>`. The child is not killed when the parent process exits.
- First run prompt: `# Context\n<context>\n\n# Task\n<task>` when a shared context is given, else
  the task text. A resume reuses the same session id with the follow-up text (default: "Continue
  the task where you stopped and finish it. Reply with the final result.").
- Exit 0 → `completed`; other exit or spawn failure → `failed` (with the error); cancellation →
  `cancelled` (the child is killed).

## 4. Depth

The extension registers no task tools when `MHC_TASK_DEPTH` (default 0) is not below
`task.max_depth` (default 1). So by default only the main session delegates.

## 5. Model choice

Order: explicit `model` → `category` (must be configured in `categories` and not disabled,
otherwise an error listing the configured ones) → the agent's models (frontmatter `model`/`models`
or the `agents.<name>` config entry; oh-my-pi `@role` selectors are skipped) → the parent
session's model. Inputs go to `resolve_delegate_model` (model-selection §5.4) with the available
models from `models.json` + `auth.json` and `system_default` = the parent model; `Deferred`/none
falls back to the parent model. No built-in category data is shipped (the defaults and chains are
left empty; categories are user-configured).

## 6. Tools

- `task`: `{task, agent?, category?, model?, name?, background?, isolated?, context?, tasks?[]}`.
  `tasks` runs a batch with the shared `context`. Foreground items return their `<task-result>`
  envelopes in order; background items return their id immediately and their envelope is later
  delivered to the session as a custom message (`task-result`, follow-up, triggers a turn).
- `task_output {id, wait?, timeout_ms?}`: envelope of a settled task or its status line; `wait`
  is clamped to `task.wait.{min,max}_ms` (default `task.wait.default_ms`).
- `task_list {}`; `task_resume {id, prompt?, background?}` (not while active); `task_cancel {id}`
  (a run owned elsewhere is signalled by pid).
- `/tasks` command: the list as a notification.

## 7. Interfaces for later phases

- `WorkspaceIsolation { prepare(id, cwd) -> {cwd, handle}, finish(workspace, succeeded) -> note }`.
  `isolated: true` (or `task.isolation.enabled`) without a backend is an error; no backend is
  shipped in this step.
- `TaskObserver { started(record), finished(record) }` for team views / terminal panes.
- Post-run hooks: a `maho_lsp::hooks::HookChain` runs over the files a child changed (git porcelain
  difference before/after the run) and its texts are appended to the envelope (`<post-run>`).

## Delta (omo v5.1.52, 2026-10-30)

Scope: the census rows routed to `crates/maho-task` (73 rows; full mapping in D6). Most rows change
machinery of the replaced code that this design does not have: a long-lived RPC host that keeps
child sessions resident, heartbeat liveness of those sessions, startup reclamation of resident
children, and an in-process child runner. Here a child is a plain `mhc -p` process (§1, §3), its
liveness is "the process has not exited", and an orphaned run is simply `interrupted` (§2). The
rows that carry user-observable behaviour are D1-D4, D7 and D8 below; D5 points to the deltas in
`workflow-dag.md` and `team.md`. Message wording is the implementer's; only the listed facts are
mandated.

### D1. Child sessions record who spawned them

Behaviour: every child run started by the runner of §3 creates its session with an ancestry
record, so the child's session file says which session and task it belongs to. The session
header written by the engine already has an optional `spawnedBy` object
`{parentSessionId, rootSessionId, taskId}` (engine interface fact, `maho-core` session header);
this delta makes delegated runs fill it.

Inputs: the task record's `owner_session` (parent), `root_session`, and `id`.

- The runner adds one environment variable to the child, next to `MHC_TASK_DEPTH`, `MHC_TASK_ID`
  and `MHC_TASK_ROOT_SESSION`: `MAHO_TASK_PARENT_SESSION=<owner session id>` (new names use the
  project's `MAHO_` prefix; the three existing `MHC_TASK_*` names stay as they are). It is needed
  because the child cannot otherwise learn its parent session: the state directory name is a
  sanitised id and `MHC_TASK_ROOT_SESSION` names the root, not the parent. `MHC_TASK_ROOT_SESSION`
  is the record's root session, or the parent when the record has none.
- When an `mhc` process that has `MHC_TASK_ID` and a non-empty `MAHO_TASK_PARENT_SESSION` creates
  a **new** session file, the header gets `spawnedBy` with `parentSessionId` = the parent
  variable, `rootSessionId` = `MHC_TASK_ROOT_SESSION` (or the parent when empty) and `taskId` =
  `MHC_TASK_ID`. An existing session file (a follow-up run, §3 resume, task-control §1 revive) is
  not rewritten; its header keeps the ancestry of the first run.
- Every later run of the same task (resume, follow-up of pending messages, team message) passes
  the same three values, so the values never change over a task's life.
- The child's own children get `parentSessionId` = the child's session id and the unchanged
  `rootSessionId`, so the root is the top-level session of the whole tree.
- The child is always the current executable, so no engine-version check is needed. If the
  variable is missing (older parent), the header simply has no `spawnedBy`; this is not an error.

Errors: none. A malformed or empty variable is treated as absent.

Tests:
1. Given a top-level session `P` that starts task `T`, when the child creates its session, then
   the session header's `spawnedBy` is `{parentSessionId: P, rootSessionId: P, taskId: T}`.
2. Given task `T` completed and is resumed, when the second run starts, then the child receives
   the same three variables and the session header is unchanged (still one header line).
3. Given a delegated child `C` (root `P`) that is allowed to spawn (`task.max_depth` 2) and starts
   `T2`, then `T2`'s header has `parentSessionId: C`, `rootSessionId: P`, `taskId: T2`.
4. Given `MHC_TASK_ID` set but `MAHO_TASK_PARENT_SESSION` empty, then the header has no
   `spawnedBy`.
5. The runner's environment for a run contains `MAHO_TASK_PARENT_SESSION` equal to the record's
   owner session (unit test over the built command).

### D2. Classifying a failed run as a credential failure

Purpose: decide whether a failed run says the child's **provider** cannot authenticate at all, in
which case another model of the same provider is pointless. Input: the failure text (record
`error` plus the tail of `<id>.log`, compared case-insensitively) and the failed model id (the
part of the record's `model` after `provider/`). Output: `none`, `rejected` or `missing`.

The classifier must recognise the credential messages that mhc itself produces (interface facts,
all in MIT crates) plus plain HTTP semantics:

- `missing` (no credential stored or signed in):
  - the engine's refusal before a request is sent: `maho-core` auth guidance ("No API key found
    for ..."), the model registry's quoted-provider variant, and the `maho-ai` adapters ("No API
    key for provider: ...", "No API key for <provider>/<model>");
  - built-in login lanes that report no usable login: a lane with no accounts left
    (`maho-ext-cursor-cli-oauth`, "no accounts: ..."), a provider that says its access token is
    absent or required (`maho-ai` cursor adapter), a "not logged in" answer;
  - a statement that the provider requires authentication, **only** when it does not name a model
    (neither the word model/models nor the failed model id): a requirement tied to one model says
    nothing about the provider's other models.
- `rejected` (a credential was sent and refused, or can no longer be renewed):
  - HTTP 401 or an "unauthorized" answer; an invalid or incorrect API key;
  - the engine's OAuth refresh failure (`maho-ai` "OAuth refresh failed for ...", an
    `invalid_grant` answer) and the engine's 'Authentication failed for "<provider>"' guidance;
  - provider-specific expiry texts that the engine emits: `maho-ext-anthropic-subscription`'s
    rejected-token text, `maho-ai` meta's "session expired ... /login" text; a required
    subscription;
  - HTTP 403 / "forbidden" only when the text is about the account, a credential, a token, a key,
    the organization or the subscription, and does not name a model (as above). Every other 403
    is `none`.
- `none`: everything else. In particular a `/login` mention by itself is not evidence (the
  engine's "No model selected" guidance mentions it too), and usage/quota/billing limits belong
  to `model-selection.md` §7.2.
- When both `rejected` and `missing` match, the result is `rejected`.

Tests (text; failed model id where it matters -> class). Providers are invented:
1. "No API key found for zeta." -> missing. "No API key for provider: zeta" -> missing.
2. "no accounts: run /login zeta-cli" -> missing. "zeta: not logged in" -> missing.
3. "zeta authentication is required" -> missing; "authentication is required for zeta-large"
   with id `zeta-large` -> none; "authentication is required to use this model" -> none.
4. "HTTP 401 from zeta" -> rejected. "OAuth refresh failed for zeta" -> rejected.
5. 'Authentication failed for "zeta". Credentials may have expired or network is unavailable.
   Run '/login zeta' to re-authenticate.' -> rejected.
6. "Meta session expired (status 401). Run `/login meta` to sign in again." -> rejected.
7. "403: the key of this account was disabled" -> rejected; "403: plan does not include
   zeta-large" with id `zeta-large` -> none; "403 access denied" -> none.
8. "No model selected." followed by the engine's login help -> none. "daily limit reached, try
   again tomorrow" -> none.
9. "401 unauthorized; no accounts: run /login zeta" -> rejected (both match).

### D3. Remedy line for a credential failure

Behaviour: when a run ends `failed` and D2 says `rejected` or `missing`, and the record's model
has a provider part, the record's `error` (and so the `<error>` of the `<task-result>` envelope)
is the original failure text followed by one new line that carries these facts:

- `missing`: no credentials were found for `<provider>`; the user can sign in (`/login
  <provider>` in an interactive session) or add the provider's API key;
- `rejected`: the credentials of `<provider>` were rejected; the user can re-authenticate
  (`/login <provider>`) or replace the API key;
- both: alternatively point the category or agent at another provider in `omo.jsonc`.

A failure without a provider part, or of class `none`, keeps its text unchanged. The line is added
once (a record that already carries it is not extended again by a later identical failure).

Tests:
1. Model `zeta/m1`, log "No API key found for zeta." → error contains the original text, the
   provider `zeta`, the word for "not found"/missing, `/login zeta` and `omo.jsonc`.
2. Model `acme/m2`, log "HTTP 401 from acme" → error says the credentials were rejected and names
   `/login acme`.
3. Model `m3` (no provider) with "401" → error unchanged. Model `a/b` failing with "exit 1" and no
   matching text → unchanged.

### D4. First-run fallback away from a provider that cannot authenticate

Behaviour: a task whose **first** run fails with a D2 credential failure before producing any
answer is started again on a model of another provider, when its model choice had further
candidates. This applies only when all of these hold:

- the model was not given explicitly (record `model_source` is `category:…`, `agent` or their
  `(fallback)` forms; never `explicit`);
- the run is run 1, it was not cancelled, and `<id>.md` is empty;
- D2 classifies the failure as `rejected` or `missing` and the model has a provider part.

Then the candidate list of §5 (the category's or agent's model references in order, then the
parent session's model) is filtered to drop every candidate on the failing provider, plus every
provider already dropped for this task, and resolved again with the same rules (§5). If a model
remains:

- the record's `model`/`thinking` become the new choice, `model_source` gets a suffix naming the
  dropped provider (for example `category:quick (fallback: openai signed out)`), the failed
  attempt's error is kept in a hook note so it stays visible in the envelope;
- the run is retried with the same first prompt in a **fresh** session id (the failed session
  contains the refused turn; this is the one exception to "the session id is fixed for the life
  of the task" in §2); `runs` stays 1;
- the retry is not a new task: same id, same output files (overwritten), no extra background
  delivery for the failed attempt.

If nothing remains, the task ends `failed` with the D3 line. Each provider is dropped at most
once, so the number of retries is bounded by the number of distinct providers in the list.
Resumes and follow-up runs are never re-routed (they continue the task's session and model).

Tests:
1. Category `c` with models `[a/one, b/two]`, `available` contains both, the child fails with
   "No API key found for a." and no output → the task is re-run on `b/two` in a new session id,
   ends `completed`, `model_source` names the dropped provider, one background delivery.
2. Same, but models `[a/one, a/two]` and no parent model → no retry; `failed` with the D3 line.
3. Explicit `model: a/one` failing the same way → no retry.
4. A failure "500 internal error" → no retry (not a credential failure).
5. A first run that wrote output before failing with "401" → no retry.
6. Candidates `[a/one, b/two, c/three]` where both `a` and `b` fail with credential errors → the
   task ends on `c/three`; neither `a` nor `b` is tried twice.

### D5. Deltas in other specs

- `workflow-dag.md` "Delta (omo v5.1.52)": the run keeps the parent session's model for every node
  dispatch and `load_skills` uses the validated bundled skills root. Its rows are mapped in
  `workflow-dag.md` D3 only.
- `team.md` "Delta (omo v5.1.52)": team members record the tree root and the lead as parent. Its
  rows are mapped in `team.md` D2 only.

### D6. Mapping table

Shared N-A reasons (referenced below):

- **R-host**: changes the resident RPC-host session machinery (open/close/park frames, host
  probes, confirmed close, host endpoint reach). Children here are `mhc -p` processes with no host
  and no resident sessions.
- **R-live**: heartbeat liveness states (unresponsive / close pending) and everything derived from
  them (notices, `unresponsive` send outcomes, error codes, snapshot fields). A child process here
  is either running or exited; a dead owner shows as `interrupted` (§2), so no liveness state
  exists to report.
- **R-reconcile**: generation-fenced startup reconciliation, residency claims, revival and
  reclamation of resident children. This design has no residency or revival; an orphaned active
  record is reported `interrupted` and can be resumed by the user.
- **R-input**: the RPC wire plumbing for per-message ids and admission reads (port methods,
  protocol frames). The user-visible part (no lost or doubled steers) is D7.
- **R-chain**: built-in agent/category fallback chains or model ids in them. This design ships no
  built-in category or agent chains (§5); categories are user-configured. Profile chain ids are
  covered by `model-selection.md` Delta.
- **R-inproc**: in-process child runner, not present (§1).

| census row | delta § | APPLIES/N-A | reason |
| --- | --- | --- | --- |
| omo-components-22 | - | N-A | R-reconcile: no session-admission gate; workflow recovery runs when the session first uses the scheduler |
| omo-components-23 | - | N-A | R-reconcile: no reconcile gate before attach; recovery is part of creating the scheduler (ancestry half: team D1) |
| omo-components-24 | - | N-A | R-live |
| omo-components-25 | - | N-A | R-live (`node_unresponsive` has no producer here) |
| omo-senpi-task-a-01 | - | N-A | R-reconcile |
| omo-senpi-task-a-02 | - | N-A | R-reconcile (per-host grouping) |
| omo-senpi-task-a-03 | - | N-A | R-reconcile |
| omo-senpi-task-a-04 | - | N-A | R-reconcile |
| omo-senpi-task-a-05 | - | N-A | R-reconcile |
| omo-senpi-task-a-06 | - | N-A | R-reconcile |
| omo-senpi-task-a-07 | - | N-A | R-reconcile |
| omo-senpi-task-a-08 | - | N-A | R-reconcile |
| omo-senpi-task-a-09 | - | N-A | R-reconcile |
| omo-senpi-task-a-10 | D8 | APPLIES | maho-task signals a recorded pid of a foreign-owned run without checking it is still the child; same hazard (signal hitting a process that is no longer ours) |
| omo-senpi-task-a-11 | - | N-A | R-reconcile |
| omo-senpi-task-a-12 | - | N-A | R-host |
| omo-senpi-task-a-13 | - | N-A | R-host |
| omo-senpi-task-a-14 | - | N-A | R-host |
| omo-senpi-task-a-15 | - | N-A | R-reconcile |
| omo-senpi-task-a-16 | - | N-A | R-live / R-host |
| omo-senpi-task-a-17 | - | N-A | R-host |
| omo-senpi-task-a-18 | - | N-A | R-live / R-host |
| omo-senpi-task-a-19 | - | N-A | R-live: no stdio RPC child and no heartbeat; process exit is the only end signal |
| omo-senpi-task-a-20 | D7 | APPLIES | identity per steer: the gateway delivery id is carried as the message identity |
| omo-senpi-task-a-21 | D7 | APPLIES | resend only when not admitted; admitted steers are not repeated |
| omo-senpi-task-a-22 | D7 | APPLIES | admission is read (from the child transcript here, not an RPC query) before any resend |
| omo-senpi-task-a-23 | - | N-A | R-input / R-host |
| omo-senpi-task-a-24 | D1 | APPLIES | child session header records parent, root and task |
| omo-senpi-task-a-25 | - | N-A | the child is always the current executable, so there is no engine-version gate (D1) |
| omo-senpi-task-a-26 | - | N-A | R-host (ancestry outcome covered by D1) |
| omo-senpi-task-a-27 | D1 | APPLIES | same observable (header ancestry); delivered through the process runner, R-inproc for the in-process path |
| omo-senpi-task-a-28 | - | N-A | typed RPC refusal codes do not exist for a `-p` child; credential failures are classified from text (D2) |
| omo-senpi-task-b-03 | D2 | APPLIES | rejected class incl. rejected OAuth token and expired session with `/login` |
| omo-senpi-task-b-04 | D2 | APPLIES | missing class and the model-scoped exception |
| omo-senpi-task-b-05 | D3 | APPLIES | remedy line differs for missing vs rejected |
| omo-senpi-task-b-06 | D3 | APPLIES | "no usable credentials" fact for a start refusal, carried by the D3 line (no typed start cause here) |
| omo-senpi-task-b-07 | - | N-A | typed RPC refusal object; see omo-senpi-task-a-28 |
| omo-senpi-task-b-08 | D4 | APPLIES | first-run fallback drops every candidate on the refusing provider |
| omo-senpi-task-b-09 | - | N-A | pure module move, no behaviour change |
| omo-senpi-task-b-10 | D1 | APPLIES | every child run carries ancestry (version gate N-A, see a-25) |
| omo-senpi-task-b-11 | - | N-A | R-host |
| omo-senpi-task-b-12 | D1 | APPLIES | header ancestry; R-inproc for the in-process path |
| omo-senpi-task-b-13 | D1 | APPLIES | resume/follow-up runs pass the same parent/root/task |
| omo-senpi-task-b-16 | - | N-A | R-live |
| omo-senpi-task-b-17 | - | N-A | R-live / R-host |
| omo-senpi-task-b-18 | - | N-A | R-host |
| omo-senpi-task-b-19 | - | N-A | R-live (no stdio heartbeat) |
| omo-senpi-task-b-20 | - | N-A | R-live |
| omo-senpi-task-b-21 | - | N-A | R-live |
| omo-senpi-task-b-22 | - | N-A | R-live: `task_send` to a running task steers or queues (task-control §1), never "unresponsive" |
| omo-senpi-task-b-23 | - | N-A | R-live |
| omo-senpi-task-b-24 | - | N-A | R-live (no unresponsive outcome to fold into workpool admission) |
| omo-senpi-task-b-25 | - | N-A | R-live |
| omo-senpi-task-b-26 | - | N-A | R-live |
| omo-senpi-task-b-27 | D7 | APPLIES | pending drain keeps the identity of each queued message and drops already admitted ones; the liveness half is R-live |
| omo-senpi-task-b-28 | D7 | APPLIES | identity on steer/follow-up (delivery id); the in-process half is R-inproc |
| omo-senpi-task-b-29 | D7 | APPLIES | identity kept through the steer-to-follow-up fallback |
| omo-senpi-task-b-30 | D7 | APPLIES | admitted / not admitted / unreadable outcomes before resend |
| omo-senpi-task-b-31 | - | N-A | R-input / R-host |
| omo-senpi-task-b-32 | - | N-A | no per-child session restore on resume: a user-paused workflow re-dispatches its paused nodes (workflow-dag §4), so one child cannot block the resume |
| omo-senpi-task-b-33 | - | N-A | R-reconcile |
| omo-senpi-task-b-34 | - | N-A | R-reconcile |
| omo-senpi-task-b-35 | - | N-A | R-reconcile |
| omo-senpi-task-b-36 | - | N-A | R-reconcile / R-host |
| omo-senpi-task-b-37 | - | N-A | R-reconcile |
| omo-senpi-task-b-38 | - | N-A | R-chain |
| omo-senpi-task-b-39 | - | N-A | R-chain (profile-side gate in model-selection Delta D1) |
| omo-senpi-task-b-40 | - | N-A | R-chain |

Totals: 18 APPLIES, 50 N-A, 68 rows in this table; 5 more APPLIES rows are mapped in
`workflow-dag.md` D3 and `team.md` D2 (each row is cited by one delta only, 2026-11-01). (Review 2026-10-30 moved a-10, a-20, a-21, a-22, b-27, b-28,
b-29, b-30 from N-A to APPLIES; see `docs/cleanroom/reviews/task-5152.md`).

### D7. Steering messages are neither lost nor doubled

Context: task-control §1.1 steers a running child by writing a gateway delivery into its inbox;
the child sends it into its session and then deletes the file; a file still present when the
run settles is moved back to the pending list. File presence alone is not proof: (a) the child
can record the message and exit before deleting the file (the follow-up run then repeats it);
(b) the child can delete the file after the send was accepted but before any turn used it, at
the very end of a `-p` run (the message is then silently lost). This delta makes the outcome
of each steer decided by the child's transcript, using the delivery id as the message's identity.

Behaviour, applied by the parent when a run that had steers written to it settles:

- Each written steer keeps its gateway `delivery_id` (in memory and, for restart safety, in the
  pending-list entry as its `id`).
- The parent reads the child's session file (`<root>/sessions/`, task session id). A steer is
  **admitted** when the transcript has a `session_control_delivery` custom message whose
  `details.delivery_id` equals the id (gateway.md §5 interface fact).
- Admitted and followed by a completed assistant message: done; any leftover inbox file is
  deleted; it is not queued again.
- Admitted but no assistant message after it: the task gets one follow-up run whose prompt asks
  the child to act on the messages it already received (the text is not repeated).
- Not admitted (file present or not): the file is deleted if present and the text goes back to
  the pending list with the same id, so it is delivered by the follow-up run exactly once.
- Transcript unreadable: treat as not admitted only when the inbox file still exists; otherwise
  report the steer as uncertain in the next `task_output`/background delivery (one line naming
  the id) rather than resending it.
- A pending-list entry whose id already appears as admitted in the transcript is dropped
  before a follow-up run starts (covers a parent restart between write and settle).

Tests:
1. Child records steer `d1` and answers, but exits before deleting the inbox file → no follow-up
   run, file removed.
2. Child deletes the file after recording `d2` but no assistant turn follows → one follow-up run;
   its prompt does not contain the `d2` text again.
3. Child never drained `d3` → file removed, one follow-up run carrying the `d3` text.
4. Parent restarted with pending entry `d4` already recorded in the transcript → `d4` is not sent
   again.

### D8. Signalling an orphaned child only while it is still that child

task-control §2 signals the recorded pid of a run owned by another process. A pid can be reused
after the child exited, so the signal could hit an unrelated process. Before signalling (cancel,
`/task-kill`, workflow cancel of a foreign-owned node), the pid must still be the task's child:
on Unix its start time is not later than the record's run start and its command line contains
the task's `--session-id <session id>`; on Windows the image is the mhc executable and the
command line contains the session id. When the check fails, nothing is signalled and the record
is marked `cancelled` (the run is already gone). Only a terminate signal is sent; there is no
escalation to a forced kill without re-checking.

Tests:
1. Record pid points to a live process whose command line lacks the session id → not signalled;
   result `cancelled` with `signalled: false`.
2. Record pid is the real child → signalled; `signalled: true`.

Leak self-check:
- [x] no fenced code block from the SUL tree (this delta has no code blocks at all);
- [x] no sentence translates or paraphrases a SUL comment, doc comment or prompt; behaviour was
      restated from the census rows and the MIT crate's own design;
- [x] the only phrase lists (D2) describe provider/engine error texts a classifier must
      recognise, written as categories with test examples, not as the SUL pattern table; env
      names, the `spawnedBy` header fields and `omo.jsonc` are interface facts;
- [x] proposed identifiers are neutral (`MAHO_TASK_PARENT_SESSION` (renamed by review from
      the writer's `MHC_` proposal), `rejected`/`missing`
      classes); no OmO-original names;
- [x] no OmO-original feature is specified (no persona chains, no mass fan-out, no
      plan-continuation state).
