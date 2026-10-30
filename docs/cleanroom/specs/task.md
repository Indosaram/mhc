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
rows that carry user-observable behaviour are D1-D4 below; D5 points to the deltas in
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
  and `MHC_TASK_ROOT_SESSION`: `MHC_TASK_PARENT_SESSION=<owner session id>` (proposed name;
  neutral). `MHC_TASK_ROOT_SESSION` is the record's root session, or the parent when the record
  has none.
- When an `mhc` process that has `MHC_TASK_ID` and a non-empty `MHC_TASK_PARENT_SESSION` creates
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
4. Given `MHC_TASK_ID` set but `MHC_TASK_PARENT_SESSION` empty, then the header has no
   `spawnedBy`.
5. The runner's environment for a run contains `MHC_TASK_PARENT_SESSION` equal to the record's
   owner session (unit test over the built command).

### D2. Classifying a failed run as a credential failure

Behaviour: a pure predicate decides whether the failure text of a run means the provider of the
child's model cannot authenticate at all, so no other model of the same provider would work
either. Input: the failure text (record `error` plus the tail of `<id>.log`, case-insensitive)
and the failed model id (the part of the record's `model` after `provider/`). Output: `none`,
`rejected` or `missing`.

- `rejected` — the provider refused a credential it was given: an HTTP 401; "unauthorized" (either
  spelling); an invalid OAuth grant or a failed OAuth refresh; a subscription that is required;
  an invalid or incorrect API key; an authentication failure or error; an OAuth token of the
  account that was rejected; a session that expired together with an instruction to run
  `/login`.
- `missing` — there is no credential at all: the engine's own "no API key found/provided for …"
  and "no API key for provider …" refusals (`maho-core`/`maho-ai` messages, interface facts);
  "not logged in"; "no … account(s)" followed by a colon or "configured"; "no … access token";
  an authentication failed code.
- A sentence that only says an access token or authentication "is required" (or an
  `authentication_required` code) counts as `missing` **unless** the text scopes it to a model
  (it contains the word "model"/"models" or the failed model id); a requirement for one model is
  not a provider-wide problem.
- An HTTP 403 or "forbidden" counts as `rejected` only when the text also names the account, a
  credential, a token, an API key, an organization or a subscription **and** does not scope
  itself to a model. Any other 403 is `none`.
- A `/login` hint on its own is not enough (the engine also mentions `/login` in unrelated
  guidance). Usage, quota and billing limits are not credential failures (they are handled by
  `model-selection.md` §7.2).
- `rejected` wins when both classes match.

Tests (given text → class):
1. "401 Unauthorized" → rejected. "Invalid API key provided" → rejected.
2. "The account's OAuth token was rejected" → rejected.
3. "Session expired (status 403). Run `/login acme` to sign in again." → rejected.
4. "No API key found for openai." → missing. "No API key for provider: openai" → missing.
5. "not logged in" → missing. "no accounts: run /login acme" → missing.
6. "Acme access token is required" → missing; "Authentication required for this model" → none;
   "authentication is required for m-large" with failed model id `m-large` → none.
7. "403 Forbidden: your organization must be verified to use this model" → none;
   "403: API key lacks access for this account" → rejected; "403 Forbidden" alone → none.
8. "No model selected. Use /login or /model" → none. "You hit your weekly limit" → none.

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
1. Model `openai/m1`, log "No API key found for openai." → error contains the original text, the
   provider `openai`, the word for "not found"/missing, `/login openai` and `omo.jsonc`.
2. Model `acme/m2`, log "401 Unauthorized" → error says the credentials were rejected and names
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
  dispatch (rows omo-components-26, omo-senpi-task-b-15) and `load_skills` uses the validated
  bundled skills root (row omo-components-03).
- `team.md` "Delta (omo v5.1.52)": team members record the tree root and the lead as parent
  (rows omo-components-27, omo-senpi-task-b-14).

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
- **R-input**: per-message client ids and admission reads over an RPC prompt channel. Steering
  here goes through gateway inbox files (task-control §1.1); a file still present at settle is the
  proof of non-delivery and is requeued, so no admission protocol exists.
- **R-chain**: built-in agent/category fallback chains or model ids in them. This design ships no
  built-in category or agent chains (§5); categories are user-configured. Profile chain ids are
  covered by `model-selection.md` Delta.
- **R-inproc**: in-process child runner, not present (§1).

| census row | delta § | APPLIES/N-A | reason |
| --- | --- | --- | --- |
| omo-components-03 | workflow-dag D2 | APPLIES | skills loaded for workflow nodes must honour the validated bundled-root override |
| omo-components-22 | - | N-A | R-reconcile: no session-admission gate; workflow recovery runs when the session first uses the scheduler |
| omo-components-23 | - | N-A | R-reconcile: no reconcile gate before attach; recovery is part of creating the scheduler (ancestry half: team D1) |
| omo-components-24 | - | N-A | R-live |
| omo-components-25 | - | N-A | R-live (`node_unresponsive` has no producer here) |
| omo-components-26 | workflow-dag D1 | APPLIES | category nodes keep the parent model as last candidate |
| omo-components-27 | team D1 | APPLIES | member ancestry (root, parent, depth) is visible in records and session headers |
| omo-senpi-task-a-01 | - | N-A | R-reconcile |
| omo-senpi-task-a-02 | - | N-A | R-reconcile (per-host grouping) |
| omo-senpi-task-a-03 | - | N-A | R-reconcile |
| omo-senpi-task-a-04 | - | N-A | R-reconcile |
| omo-senpi-task-a-05 | - | N-A | R-reconcile |
| omo-senpi-task-a-06 | - | N-A | R-reconcile |
| omo-senpi-task-a-07 | - | N-A | R-reconcile |
| omo-senpi-task-a-08 | - | N-A | R-reconcile |
| omo-senpi-task-a-09 | - | N-A | R-reconcile |
| omo-senpi-task-a-10 | - | N-A | R-reconcile (no claim to re-check; orphan cancel only signals a live pid, task-control §2) |
| omo-senpi-task-a-11 | - | N-A | R-reconcile |
| omo-senpi-task-a-12 | - | N-A | R-host |
| omo-senpi-task-a-13 | - | N-A | R-host |
| omo-senpi-task-a-14 | - | N-A | R-host |
| omo-senpi-task-a-15 | - | N-A | R-reconcile |
| omo-senpi-task-a-16 | - | N-A | R-live / R-host |
| omo-senpi-task-a-17 | - | N-A | R-host |
| omo-senpi-task-a-18 | - | N-A | R-live / R-host |
| omo-senpi-task-a-19 | - | N-A | R-live: no stdio RPC child and no heartbeat; process exit is the only end signal |
| omo-senpi-task-a-20 | - | N-A | R-input |
| omo-senpi-task-a-21 | - | N-A | R-input |
| omo-senpi-task-a-22 | - | N-A | R-input |
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
| omo-senpi-task-b-14 | team D1 | APPLIES | nested team members record the tree root |
| omo-senpi-task-b-15 | workflow-dag D1 | APPLIES | parent model persisted in the run and used by start, retry and recovery |
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
| omo-senpi-task-b-27 | - | N-A | R-input / R-live |
| omo-senpi-task-b-28 | - | N-A | R-input / R-inproc |
| omo-senpi-task-b-29 | - | N-A | R-input |
| omo-senpi-task-b-30 | - | N-A | R-input |
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

Totals: 15 APPLIES, 58 N-A, 73 rows.

Leak self-check:
- [x] no fenced code block from the SUL tree (this delta has no code blocks at all);
- [x] no sentence translates or paraphrases a SUL comment, doc comment or prompt; behaviour was
      restated from the census rows and the MIT crate's own design;
- [x] the only phrase lists (D2) describe provider/engine error texts a classifier must
      recognise, written as categories with test examples, not as the SUL pattern table; env
      names, the `spawnedBy` header fields and `omo.jsonc` are interface facts;
- [x] proposed identifiers are neutral (`MHC_TASK_PARENT_SESSION`, `rejected`/`missing`
      classes); no OmO-original names;
- [x] no OmO-original feature is specified (no persona chains, no mass fan-out, no
      plan-continuation state).
