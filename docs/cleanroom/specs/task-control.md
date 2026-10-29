# Task control and team tasklist (behaviour spec, Phase 4)

Scope: the controls on delegated tasks that `task.md` and `team.md` do not cover:

- messaging a child task that is still running (steer) or has finished (revive);
- the full cancel contract and the interactive `/task-kill` command;
- a shared **team tasklist**: work items a team lead creates and that members claim and finish,
  with dependencies.

Everything else (spawning, records, `task_output`, `task_resume`, teams, members, `team_message`,
`team_stop`) is as in `task.md` and `team.md`; this spec only adds to them.

Sources. Written by the spec writer after reading the SUL crates `senpi-task`, `team-core` and
`maho-omo-task` for understanding only (README "Roles"). No code, comment, prompt, tool
description or message text was carried over. Statuses, record fields and file layout follow the
MIT `task.md`/`team.md` model, not the SUL one. Names are neutral.

## 1. `task_send`: message a child

Parameters: `to` (required: task id or task name, or a team member as `<team>/<member>`),
`message` (required, non-empty string), `all_sessions` (boolean, default false).

Resolution: `to` is looked up as a task id, then as a task name in the caller's state directory.
`<team>/<member>` (or `<team>/*` for every member) is passed to `team_message` of `team.md` and its
result is returned unchanged. Nothing found -> `not_found`, with the list of ids the caller could
mean (active tasks of the session, at most 10).

Ownership: a task belongs to the session that spawned it. A caller that is neither that session
nor its root session gets `scope_denied` naming the owning session, unless `all_sessions: true`.

Behaviour by the task's state (one outcome per call, reported as `kind` in the tool details):

| Task state | Outcome | Effect |
| --- | --- | --- |
| `queued` (not started yet) | `queued` | message appended to the task's pending list; position returned (1-based). The first run starts with the task text followed by the pending messages. |
| `running`, live channel to the child | `steered` | message delivered into the child's current turn as a steering message (§1.1) |
| `running`, no live channel | `queued` | appended to the pending list; delivered as one follow-up run (messages joined by blank lines, in order) when the run settles |
| `completed`, `failed`, `interrupted` | `revived` | same as `task_resume { id, prompt: message, background: true }`; the new run number is returned |
| `cancelled`, or a cancel is in progress | `not_continuable` | nothing is sent; the text says the task was cancelled and suggests spawning a new task |
| another send or cancel on the same task is in progress | `not_continuable` | the text says to retry; no state changes |

Limits: `message` larger than 32 KiB (UTF-8 bytes) is rejected as `invalid_arguments` with a hint
to write the content to a file and send its path. An empty or whitespace-only message is
`invalid_arguments`.

### 1.1 Steering channel

A running child is a non-interactive mhc session with a fixed session id (`task.md` §2). Steering
uses the session gateway (`gateway.md`): the parent writes a delivery to the child session's
inbox with `deliver_as: steer`, `hop: 1` and `sender {"kind":"agent","session_id":<parent>}`.

This needs one addition to `gateway.md` §5, which excludes `-p` runs from receiving: a delegated
run (one started with `MHC_TASK_ID` set) is an exception. When the gateway is enabled it writes
its endpoint record, creates its inbox and drains it exactly as §5 describes, and removes the
record when the run ends. Other `-p` runs still do not receive.

The channel counts as live when the gateway is enabled and the child has a live endpoint record.
No channel (gateway disabled, no endpoint) or a write error falls back to the `queued` row above;
it is never an error. When the run settles and a steer delivery the parent wrote is still in the
child's inbox (not drained in time), the parent removes that file and moves its text to the
pending list, so the message is delivered by the follow-up run instead of being lost.

### 1.2 Pending list

The pending list is stored in the task record as `pending_messages` (array of
`{id, text, created_at}`), written atomically with the record. It survives a restart of the
parent. Cancelling the task drops it; the cancel result then says how many messages were not
delivered.

## 2. `task_cancel` (extends `task.md` §6)

Parameters: `id` (task id or name, required), `reason` (optional string, recorded as the record's
`error`).

| Task state | Outcome | Effect |
| --- | --- | --- |
| `queued` | `cancelled` | never started; status `cancelled` |
| `running` (owned here) | `cancelled` | child killed; status `cancelled`; pending list dropped |
| `running` (owned by another process) | `cancelled` | the owner pid is signalled; status `cancelled` once it exits |
| `cancelled` | `noop` | text says it is already cancelled |
| `interrupted` | `cancelled` | owner process is already gone, nothing to kill; status `cancelled`; pending list dropped |
| `completed`, `failed` | `noop` | text names the unchanged status |
| unknown id | `not_found` | |

Result text names the task, the previous and the new status. Cancel is final: `task_send` never
revives a cancelled task (§1). `task_resume` stays allowed as an explicit user choice.

## 3. `/task-kill` command

1. Collect this session's tasks in `queued`, `running` or `interrupted` state.
2. None -> one info notification saying there is nothing to cancel (wording free); no dialog
   opens.
3. Otherwise open a select dialog with one option per task: `<name> (<id>) <status>`, or
   `<id> <status>` when the task has no name distinct from its id.
4. Dismissed -> nothing happens.
5. A confirm dialog names the chosen task id; declined -> nothing happens.
6. Confirmed -> `task_cancel` with reason `/task-kill`, then an info notification naming the id.
   A cancel error is shown as an error notification.

In a session without an interactive UI the command lists the candidates and says to use
`task_cancel`.

## 4. Team tasklist

A per-team list of work items. The list is records only: no tasklist tool starts or controls a
child task; members are started with `task`/`team.md` as before.

### 4.1 Storage

`<task state dir>/teams/<team>.tasks/` next to the team file of `team.md` §1:

- `<n>.json`: one item; `n` is a decimal id starting at 1;
- `next-id`: the last allocated id (text). Ids are never reused, even after `deleted`;
- `<n>.claim`: exclusive claim marker (§4.4).

Directories are created with mode 0700 on Unix. Writes are atomic (temp file + rename) and id
allocation holds an exclusive lock on the directory, so concurrent creates get distinct ids.

Item fields: `id`, `subject`, `description`, `status`, `owner` (optional member name or `lead`),
`blocked_by` (array of item ids), `created_at`, `updated_at`, `claimed_at` (ms timestamps; the
last only once claimed).

### 4.2 Statuses and transitions

Statuses: `pending`, `claimed`, `in_progress`, `completed`, `deleted`.

Allowed moves: `pending -> claimed`, `claimed -> in_progress`, `in_progress -> completed`, and
`<any status> -> deleted`. Setting the current status again is a no-op success.
`pending -> in_progress` claims first and then moves on, as one call. Every other move fails with
`invalid_transition` naming both statuses.

Owner rule: any move except to `deleted` must come from the item's owner; otherwise
`cross_owner`. The owner is set when an item is claimed.

### 4.3 Dependencies

An item may be claimed only when every id in `blocked_by` that exists is `completed`. A
`blocked_by` id with no item file is ignored. A blocked claim fails with `blocked_by`, listing the ids still open.

### 4.4 Claim

Claiming a `pending` item sets `status: claimed`, `owner`, `claimed_at`. Two concurrent claims of
the same item: exactly one wins, the other gets `already_claimed`. The marker file is created
exclusively; a marker older than 5 minutes is stale and may be replaced. Claiming an item that is
not `pending` fails with `already_claimed`.

### 4.5 Tools

Registered with the team tools (same depth gate). `team` is optional when the session has exactly
one team, as in `team.md`.

- `team_task_create {team, subject, description, blocked_by?}`: new `pending` item; result names
  the id, subject and blockers.
- `team_task_list {team, status?, owner?}`: items sorted by numeric id, filtered by exact status
  and owner; first line is the count. Item files that do not parse are skipped (logged), never an
  error.
- `team_task_get {team, id}`: all fields; a missing item is `not_found` (a result, not a tool
  error).
- `team_task_update {team, id, status, owner?}`: applies §4.2-4.4. `owner` defaults to `lead`
  (the calling lead session); a value other than `lead` must be a member of the team, otherwise
  `invalid_arguments`.

Update failures are results with `kind` = `already_claimed`, `blocked_by`, `invalid_transition` or
`cross_owner` and a one-line reason; storage errors are tool errors. Long subjects and owners are
shortened in the text output (whitespace collapsed to single spaces, cut at a fixed length of the
implementer's choice); the details carry the full item.

## 5. Platform notes

- Unix permissions apply only where the platform has them; Windows uses default ACLs.
- Signalling a foreign owner (§2) uses the platform's process termination; a pid that no longer
  exists counts as already stopped.

## 6. Acceptance tests

1. `task_send` to a `queued` task returns `queued` with positions 1 then 2; the first run's prompt
   contains the task text followed by both messages in order.
2. `task_send` to a running child with a live gateway endpoint writes one inbox delivery with
   `deliver_as: steer` and `hop: 1` and returns `steered`; without an endpoint, or with the
   gateway disabled, it returns `queued` and the message runs as a follow-up after the run
   settles. A delegated run with `MHC_TASK_ID` set writes an endpoint record while it runs and
   removes it at exit; a plain `-p` run does not. A steer still in the inbox when the run settles
   is removed and delivered by the follow-up run exactly once.
3. `task_send` to a completed task returns `revived`, the run count grows by 1 and the session id
   is unchanged.
4. `task_send` to a cancelled task returns `not_continuable`; the record is unchanged.
5. A caller from another session gets `scope_denied`; the same call with `all_sessions: true`
   succeeds.
6. A 33 KiB message is `invalid_arguments`; an unknown id is `not_found`.
7. `<team>/<member>` and `<team>/*` produce the same result as `team_message`.
8. `task_cancel` on a running task kills the child, sets `cancelled` with the reason, and reports
   the dropped pending-message count; a second cancel is `noop`; a completed task is `noop`; an
   `interrupted` task becomes `cancelled` without any process being signalled.
9. `/task-kill` with no active task notifies and opens no dialog; dismissing the select or
   declining the confirm cancels nothing; confirming cancels exactly the chosen id with reason
   `/task-kill`; only `queued`, `running` and `interrupted` tasks are offered.
10. Tasklist ids are 1, 2, 3 across creates; deleting 2 and creating again gives 4; ten concurrent
    creates give ten distinct ids.
11. Every allowed transition of §4.2 succeeds; `claimed -> pending`, `completed -> in_progress`
    and `pending -> completed` fail with `invalid_transition`; repeating the current status is a
    no-op success.
12. `pending -> in_progress` sets owner and `claimed_at` in one call.
13. An item blocked by an open item fails with `blocked_by` listing it; after the blocker is
    `completed` the claim succeeds; a `blocked_by` id that does not exist does not block.
14. Two concurrent claims: one success, one `already_claimed`; a stale marker older than 5
    minutes does not prevent a claim.
15. A move by a non-owner fails with `cross_owner`; `deleted` by a non-owner succeeds.
16. `team_task_list` skips a corrupt item file and filters by status and owner;
    `team_task_get` of a missing id returns `not_found`.
