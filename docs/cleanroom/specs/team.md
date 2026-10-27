# Team runs and terminal panes (behaviour spec)

Phase 3 step 2 of the MIT migration. Scope: running several delegated tasks as one **team**
(create, list members, message a member, wait/collect, stop) on top of `maho-task`, and showing
delegated tasks in **terminal multiplexer panes** with a headless fallback.

Sources. Written from MIT material only:

- `can1357/oh-my-pi` @ f59bbac3 (MIT): terminal multiplexer detection
  (`packages/tui/src/terminal-multiplexer.ts`, `multiplexers/tmux.ts`), pane/window launch
  (`packages/coding-agent/src/subprocess/terminal-launch/{tmux,cmux,shared,types}.ts`), the
  fake-CLI test approach (`packages/tui/test/helpers/fake-tmux.ts`), the batch concurrency helper
  (`packages/coding-agent/src/task/parallel.ts`).
- `tmustier/pi-agent-teams` @ 235fac86 (MIT): team model (leader + named members, direct
  messages, completion notifications to the leader with progress counters, a "whole batch done"
  notification, stop/done lifecycle).
- `HazAT/pi-interactive-subagents` @ c100577e (MIT): split off the parent's own pane (`$TMUX_PANE`)
  without stealing focus (`split-window -d`), close with `kill-pane`, read a pane with
  `capture-pane`, backend preference variable, "inside tmux = `$TMUX` set and `tmux` on PATH".
- `JokerQianwei/pi-tmux` @ 53af7311 (MIT): visible named children that the parent waits on.
- mhc's own MIT code: `maho-task` (the `TaskObserver` trait, `TaskManager`, resume), `maho-config`
  (`task.team`, `teams` keys in config-keys.md).

Parts with **no original** (fixed here, implemented from this spec): mapping teams onto
non-interactive `mhc -p` children (members are tasks; a message is a follow-up run), the queued
message drain, the per-team concurrency/wall-clock limits from the existing `task.team` keys,
templates from the existing `teams.<name>` keys, and the environment variables of §5.

Naming is neutral: "team", "member", "lead". No names from other products' team features.

## 1. Model

- A **team** has a name (`^[a-z0-9-]+$`, at most 32 chars), an optional shared `context`, and an
  ordered list of **members**. It belongs to the task state directory of the session that created
  it (the same directory `task` uses) and is persisted as `teams/<team>.json` there.
- A **member** has a name (same pattern, unique in the team), the assignment text (`task`), and
  the usual task options (`agent`, `category`, `model`, `isolated`), plus `pane` (§4). Each member
  is backed by exactly one `maho-task` task; its task id is `<team>-<member>` (allocated through
  the normal id allocator, so a repeat gets `-2`).
- The task record of a member carries `group = <team>` and `quiet = true`.
  - `group` lets observers and listings recognise team members.
  - `quiet` suppresses the generic "background task settled" follow-up of `maho-task`; the team
    sends its own notifications (§3.5) instead.
- The session that created the team is the **lead**. Members cannot see the lead's conversation;
  everything they need is in `context` + their `task`.

Member state (derived, not stored): `pending` (not started yet because of the team's parallel
limit), `queued`/`running` (task active), `idle` (task settled: completed, failed, cancelled or
interrupted) and `stopped` (stopped by `team_stop`). A member with undelivered messages is
reported with `messages: N`.

## 2. Tools (MIT profile)

All tools are registered together with the task tools (same depth gate: a session that cannot
spawn tasks gets no team tools). Errors are tool errors with a one-line message.

### 2.1 `team_create`

Parameters: `name` (required unless `template`), `context`, `members` (array of
`{name, task, agent?, category?, model?, isolated?, pane?}`), `template` (name of a configured
`teams.<name>` entry, §6), `panes` (boolean, overrides §4 for this team).

Behaviour:

1. Validate names; reject an existing *live* team with the same name in this state directory
   (a settled or stopped team of the same name is replaced).
2. Reject more than `task.team.max_members` members (default 8) and an empty member list.
3. Build every member's spawn request first (agent lookup, model choice — same rules as `task`);
   any error aborts before anything starts.
4. Start the first `task.team.max_parallel_members` (default 4) members as background tasks; the
   rest stay `pending` and start as earlier members settle.
5. Persist the team, start its driver (§3) and return one line per member
   (`<member> → task <id> (<state>)`) plus a sentence saying results arrive as team updates and
   that `team_wait` collects them.

### 2.2 `team_members`

Parameters: `team` (optional when exactly one team exists in this state directory), `tail`
(number of trailing output lines per member, default 0, max 40).

Lists teams when `team` is omitted and several exist. For a team: its state (`running`,
`settled`, `stopped`, `timed out`), then per member: name, task id, state, agent, model, runs,
undelivered message count, pane id when shown, and — when `tail > 0` — the last lines of the
member's output (or of its log while the output is empty). This is the headless "live tail".

### 2.3 `team_message`

Parameters: `team`, `member` (a member name, or `*` for all members), `message` (required).

- Member `idle` → the member's task is resumed at once in the background with the message as the
  follow-up (same session, full context).
- Member `queued`/`running`/`pending` → the message is appended to the member's queue and
  delivered when its current run settles (all queued messages joined with blank lines, in order,
  as one follow-up run).
- Member `stopped` → error.
- Result text says which happened (`delivered (run N started)` or `queued (N waiting)`).

### 2.4 `team_wait`

Parameters: `team`, `members` (optional subset), `timeout_ms` (clamped like `task_output`'s wait:
`task.wait.*`).

Waits until every selected member is `idle` or `stopped` with an empty queue (or the timeout
passes), then returns each selected member's current `<task-result>` envelope, prefixed by a
summary line `team <name>: X/Y done[, Z failed][, timed out after …]`. A successful full wait marks
the team **collected**: the driver then does not send the "team finished" notification (§3.5).

### 2.5 `team_stop`

Parameters: `team`, `member` (optional; all members when omitted).

Cancels the member tasks that are active, drops their queues and pending starts, marks them
`stopped`, closes their panes. Stopping every member marks the team `stopped` and ends its
driver. Returns one line per member with what happened.

### 2.6 `/team` command

`/team` shows the `team_members` listing (without tails) of every team in the session's state
directory.

## 3. Driver

One driver per live team (a tokio task in the lead process). It loops every 250 ms or when woken
by `team_message`/`team_stop`:

1. For each member whose task settled since the last pass: if it has queued messages, resume it
   with them (background, quiet) and clear the queue; otherwise record the settlement.
2. Start `pending` members while fewer than `max_parallel_members` are active.
3. If `max_wall_clock_minutes` (default 120) has passed since creation, stop every member with the
   reason `timed out` (state `timed out`).
4. When every member is idle/stopped with empty queues the team is **settled**; the driver keeps
   the team (a later `team_message` wakes it again) but stops polling until woken.

### 3.5 Notifications to the lead

Sent as custom messages (`custom_type = "team-update"`, displayed, follow-up delivery that
triggers a turn), only when the session has a message sender:

- Per settled member run (not when it is immediately resumed with queued messages):
  `[team <name>] <member> <completed|failed|cancelled|interrupted> (task <id>, run N)`, the first
  500 characters of the output, and a progress line `Progress: D/T done (R running, P pending)`.
- When the whole team settles for the first time after a change and the lead has not collected it
  with `team_wait`: `[team <name>] all T members finished (S succeeded, F failed). Use team_wait to
  collect the results.`

## 4. Panes

A `TaskObserver` shows delegated tasks in terminal multiplexer panes.

### 4.1 Detection (first match)

1. `MAHO_TEAM_PANES=off` → headless.
2. Backend preference `MAHO_TEAM_MUX=tmux|cmux` → that backend if available, else headless.
3. tmux available: `$TMUX` non-empty and a `tmux` executable on `PATH`.
4. cmux available: `$CMUX_WORKSPACE_ID` or `$CMUX_SURFACE_ID` set and `cmux` on `PATH`.
5. Otherwise headless. Headless means: no panes; everything else (tools, notifications, tails in
   `team_members`) works the same.

### 4.2 Which tasks get a pane

`MAHO_TEAM_PANES`:

- `auto` (default): team members, when a backend is available.
- `all`: every delegated task (`task` included), when a backend is available.
- `off`: none.

Per team, `team_create.panes` overrides (`true` = members get panes when a backend exists,
`false` = none). Per member, `pane: false` opts out, and a template member with
`backendType: "tmux"` asks for a pane even when the team default would not. The effective choice
is recorded on the task (`pane`); a task's own `pane` wins over the mode except `off`.

### 4.3 Opening (task starts running)

The pane runs a read-only follower of the task's files — never the child itself (the child keeps
running as the normal `mhc -p` process, so headless and pane runs behave identically):

`/bin/sh -c 'printf "%s\n\n" <title>; tail -n +1 -F <log> <output> & t=$!; while kill -0 <lead pid>
2>/dev/null; do sleep 1; done; kill $t'` with the title and paths single-quoted. The loop ends the
pane once the lead process is gone, so panes never outlive the session that opened them (a
background child may keep running; its files stay on disk). `<title>` is
`team <team> · task <id>` or `task <id>`. The log and output files are created empty first.

tmux (placement `MAHO_TEAM_PANE_LAYOUT=split` default, or `window`):

- split: the first pane of the process is `split-window -d -h` off `$TMUX_PANE` (right of the lead,
  focus stays with the lead); later panes are `split-window -d -v` off the most recent pane still
  open (stacked on the right). If a split fails (for example no space), fall back to a window.
- window: `new-window -d -n <short title>`.
- Always `-c <cwd>` with `#` doubled (tmux format-expands it), `-P -F '#{pane_id}'` (or
  `#{window_id}` for windows) and the command as separate arguments after `--` so tmux executes it
  directly. A reply that is not `%N` (`@N` for windows) is an error.
- The pane title is set with `select-pane -t <pane> -T <title>` (best effort).

cmux: `cmux --json new-split right [--surface $CMUX_SURFACE_ID] --command <shell text>` and the
id from the JSON reply (`pane_id`/`pane_ref`, also under `data`); close with
`cmux close-surface --surface <id>`.

Open failures never fail the task: the error is kept per task (shown by `team_members` as
`pane unavailable (<error>)`) and the task runs headless.

### 4.4 Closing (task settles)

After `MAHO_TEAM_PANE_LINGER` seconds (default 3, `0` = immediately), `kill-pane -t <pane>`
(`kill-window -t <window>` for windows). A resumed member gets a new pane for the new run.
`team_stop` cancels the members it stops; their panes close as their tasks settle (same linger). Close errors are ignored (the user
may have closed the pane).

## 5. Environment variables

| Variable | Values | Default |
|---|---|---|
| `MAHO_TEAM_PANES` | `auto`, `all`, `off` | `auto` |
| `MAHO_TEAM_MUX` | `tmux`, `cmux` | detect (§4.1) |
| `MAHO_TEAM_PANE_LAYOUT` | `split`, `window` | `split` |
| `MAHO_TEAM_PANE_LINGER` | seconds (integer) | `3` |

These are environment variables rather than config keys because the config schema
(config-keys.md) strips unknown keys.

## 6. Configuration (existing keys)

- `task.team.max_members` (1..8, default 8), `task.team.max_parallel_members` (1..8, default 4),
  `task.team.max_wall_clock_minutes` (>0, default 120): limits of §2.1 and §3.
- `teams.<name>`: templates for `team_create { template }`. Mapping: the team name is the entry's
  `name` (or the map key); `description` becomes the context when `context` is not given; each
  member `{name, prompt, kind}`: `kind: "category"` → `category`; `kind: "subagent_type"` →
  `agent = subagent_type` (members without a `prompt` are rejected: they would have no task);
  `cwd` → the member's working directory (relative to the session cwd); `worktreePath` present →
  `isolated: true`; `isActive: false` → skipped; `backendType: "tmux"` → `pane: true`. Other
  fields are ignored.

## 7. Interfaces

- `maho-task`:
  - `TaskRecord` gains `group: Option<String>`, `pane: Option<bool>` (pane preference; `None` =
    the observer's default) and `quiet: bool` (all default when absent in old records, omitted
    when unset); `SpawnRequest` gains the same three fields; the `task` tool accepts `pane`.
  - `TaskObserver` gains `started_in(record, state)` / `finished_in(record, state)` with default
    bodies calling `started`/`finished`; the manager calls the `_in` forms so an observer can find
    the task's output and log files.
  - A quiet task's settlement is not passed to the background-delivery callback.
  - `TaskAddon` (tools layered on the task machinery) and `TaskScope` (per call: the session's
    `TaskManager`, the config view, request building with the `task` rules, and a message sender).
    `TaskExtension.addons` lists the addons whose tools are registered with the task tools.
- `maho-team` (new crate): `TeamAddon` (the tools of §2 and the command of §2.6), `PaneObserver`
  (§4), `Multiplexer` detection, `TmuxBackend`, `CmuxBackend`, `PaneSettings::from_env`.
- `maho-cli` MIT profile: `cli/team_view.rs` builds the observer and the addon and passes them to
  the task extension; the default (`omo`) build is unchanged.

## 8. Acceptance (tests written from this spec)

1. `team_create` with three members and `max_parallel_members = 2` starts two, the third starts
   after one settles; every member record has `group` and `quiet`.
2. No generic task-result follow-up is sent for a quiet task; one team update per member and one
   "all finished" update are sent when not collected; no "all finished" update after `team_wait`.
3. `team_message` to an idle member resumes it (runs + 1, same session); to a running member
   queues it and the driver resumes it after the run with the joined messages.
4. `team_wait` with a timeout returns the summary with `timed out`; `team_stop` cancels active
   members and marks the team stopped; messaging a stopped member is an error.
5. Templates map as §6; `max_members` is enforced; invalid names are rejected.
6. tmux backend argv (split first/next, window, `#` escaping, `-d`) is exactly as §4.3 against a
   fake `tmux` script; with a real tmux server (when available) a pane is opened, shows the task
   output, and is closed after the task settles.
7. Detection honours `MAHO_TEAM_PANES`, `MAHO_TEAM_MUX`, `$TMUX`, PATH lookup; headless when absent.
8. The MIT smoke runs a two-member team with real `mhc -p` children (headless, and in a detached
   tmux session when tmux is available) and collects both results.

## Leak self-check

- [x] no SUL file was opened while writing this spec (`team-core`, `tmux-core`, `senpi-task`, the
      omo-gated `maho-cli` modules and `crates/omo/**` were not read);
- [x] no OmO-original concept or name appears; identifiers are neutral (`maho-team`, `team_*`
      tools, `team-update`, `MAHO_TEAM_*`); the config keys of §6 are interface facts already
      listed in config-keys.md;
- [x] wording is new; message texts are written here, not copied from the MIT originals.
