# Side panel — behavior spec (Phase 5)

Scope: an optional read-only status panel shown in the interactive TUI next to (or above) the
transcript. It summarizes the session, delegated tasks, recent tool calls, changed files and
related state, and lets the user open detail views. It replaces `maho-omo-side-panel`. The
config keys are interface facts from `config-keys.md` (`side_panel`). The panel never changes
session state; every view is derived from host events, task records and `git`.

## 1. Enabling

- Config key: `side_panel.enabled` (default `false`).
- Per-run override: a boolean CLI flag (interface fact: `--omo-side-panel`; an implementation may
  add a neutral alias such as `--side-panel`). `--omo-side-panel` forces it on,
  `--omo-side-panel=false` (or the host's negation form) forces it off; the flag wins over config
  in both directions.
- The flag is always registered. Everything else (command, data readers, timers) is only set up
  when the panel is enabled for the session. A disabled session must not start any reader.
- The panel only appears in interactive TUI mode with a UI. In print/JSON/RPC modes, or when the
  host context has no UI, it stays dark without error.

## 2. Settings (interface facts, see `config-keys.md`)

| Key | Type / bounds | Default |
| --- | --- | --- |
| `side_panel.enabled` | boolean | `false` |
| `side_panel.width` | integer 24..160 columns, or string `"NN%"` with exactly two digits, 10..50 | `"26%"` |
| `side_panel.min_columns` | integer 60..400 | `120` |
| `side_panel.clickable` | boolean | `true` |
| `side_panel.usage_poll_seconds` | integer 60..3600 | `150` |
| `side_panel.sections.{session,goal,context,usage,agents,tools,files,memory}` | boolean | all `true` except `usage` (`false`) |

- Unknown keys inside `side_panel` or `side_panel.sections`, wrong types, or out-of-range values
  are a validation error that names the offending field (`side_panel.width`, ...). At startup the
  error is shown as a warning and the panel stays off for that session; the session still runs.
- Settings are re-read on each session start, so a config change takes effect on the next session.

## 3. Layout and width

- Effective width for terminal width `T`: columns value as given, or `floor(T * pct / 100)`; then
  clamped to 32..80. If `T - 60` (60 = minimum transcript width) is below 32 the panel uses 32,
  otherwise it is capped at `T - 60`. Unknown `T` (0) yields the clamped value.
- The column is only visible while the terminal is at least `side_panel.min_columns` wide; below
  that it is hidden (not unmounted) and reappears when the terminal grows.
- Column mode needs the fullscreen TUI layout. In the default (non-fullscreen) TUI mode the panel
  falls back to a block rendered above the editor, and the user gets a one-time informational
  notice naming the setting/flag that enables fullscreen mode. "One time" is per user: a marker
  file in the agent home is created exclusively (mode 0600 on Unix); if it already exists or
  cannot be created, no notice is shown. If the host later switches to fullscreen, the block is
  removed and the column is mounted instead.
- If the host rebuilds its layout and drops the column, the panel re-attaches on the next repaint.

## 4. Rendering rules

- Output is a list of rows, each padded or truncated to exactly the panel width in terminal
  cells. Truncation ends with `…` (no ellipsis at width 1); width 0 renders nothing.
- Width counting: SGR color sequences count 0; combining marks, ZWJ and variation selectors 0;
  East Asian wide/fullwidth and emoji ranges 2; others 1.
- All text from data sources is sanitized before rendering: bidi override/isolate/mark characters
  are removed; tab, newline, CR, VT, FF, NEL become a space; other control characters (including
  ESC and BEL) are removed. Theme color is applied after sanitizing and before padding.
- Sections appear in this order, each only when enabled and non-empty, separated by exactly one
  blank row: session, goal, context, usage, agents, tools, files, memory, then a location row that
  is always shown.
- Path tails are truncated from the start (`…tail`) so the end of a path stays visible.

## 5. Sections

- **Session**: heading `SESSION` with `<cost> · <tokens>` when either is non-zero; model name
  (last `/` segment of the model id); elapsed time since the panel mounted; `in X · out Y` token
  totals and latest cache hit rate when known; spend of finished delegated tasks when non-zero.
  Cost = parent session cost + delegated task spend.
- **Goal** (when the host exposes a goal state file): heading `GOAL <status>[ · elapsed]` with
  status one of active, paused, blocked, limited by budget, complete; the objective (clickable);
  tokens used; a budget bar with percent when a token budget is set; continuation counts when
  non-zero. Malformed files or unknown statuses hide the section; the file is re-parsed only when
  its mtime or size changes.
- **Context**: heading `CONTEXT used/window` and a usage bar with percent. When the window is
  known but the token count is not, show a "measuring" placeholder; never print a missing count
  as 0.
- **Usage** (off by default): subscription plan limit windows per signed-in provider account
  (percent bar, reset time, last update age, error). Polled every `usage_poll_seconds`, and also
  refreshed after turns. Missing credentials or a failed poll hide the rows; they do not error.
- **Agents**: delegated tasks whose parent is the current session (from the `maho-task` state
  directory, `task.md` §2). Heading shows `N running · N parked · N done`. One clickable row per
  task: status glyph, name (first non-empty of description / name / agent / id), elapsed or total
  duration, and current activity while running. Status mapping: queued, running, completed →
  finished, failed, cancelled, interrupted → parked (shown with a distinct glyph and no duration,
  plus the reason). The list keeps at most 12 tasks: when over capacity, the oldest finished one
  is dropped; active tasks are never dropped. A task's cost is added to the spend once, when it
  first reaches a terminal state. Partial updates keep previously known fields.
- **Tools**: tool calls started since the last user input, newest kept, at most 24 stored and 6
  shown with a `+N earlier` count. Each row is the tool name plus the first non-empty string
  argument among `file_path`, `path`, `command`, `pattern`, `query`, `url`. User input clears it.
- **Files**: `git status` of the repository containing the cwd (porcelain v1, NUL-separated,
  untracked files normal). Heading `N changed[ · +M more]`; at most 8 rows of
  `<XY> <basename>[ +A/-R]`, where A/R sum staged and unstaged numstat (binary = 0/0, untracked
  = no counts). Renames show the destination. Color class: untracked dim, conflicts error,
  staged-only success, others normal. Rows are clickable. Not a git repository → no section.
- **Memory**: the resolved memory store identity in the heading plus a few status rows (queued
  facts, recall counts). Hidden when `memory.enabled` is `false` or nothing resolves.
- **Location**: cwd with `$HOME` shown as `~`, plus ` · <branch>` (or 7-char detached HEAD;
  linked worktrees are followed through the `.git` file).

## 6. Refresh lifecycle

- Session start: read settings, mount, then load tasks, git (forced) and memory (forced) in the
  background. Turn end, agent settle and message end: update host facts (model, usage, totals,
  goal file) and repaint, then refresh tasks, git and memory. Tool start: record the call, then
  refresh. Tool end: refresh git only.
- Throttles: git at most once per 2 s and memory once per 5 s unless forced. While any task is
  queued or running, repaint once per second so durations tick; the timer stops when none is.
- Each git command runs with a 5 s timeout. Failures keep the previous data.
- Session shutdown: stop timers and pollers, unmount (restore the host's original layout and
  link handler), clear all state. Results of background work that finish after shutdown, or that
  belong to an earlier session, are discarded.
- An unreadable task state directory is logged at debug level and leaves the list unchanged.

## 7. Actions and detail views

- When `clickable` is true and the terminal supports OSC 8 hyperlinks, clickable rows are wrapped
  in a link with a private URL scheme (proposed: `mhc-panel:<kind>/<percent-encoded value>`,
  kinds `file`, `agent`, `goal`, `memory`; goal and memory use the value `current`). The panel
  claims only well-formed URLs of its scheme; any other URL goes to the previous handler.
  Malformed escapes, empty values, invalid UTF-8 or unknown kinds are not claimed.
- Opening `file`: a read-only diff viewer titled with the path. Tracked files use
  `git --literal-pathspecs diff HEAD -M -- [<from>] <path>`; untracked files diff against
  `/dev/null` with `--no-index`; an untracked directory lists up to 50 untracked files under it and
  notes how many more exist. Output is capped at 5000 lines plus a `... N more lines` row. Empty
  output shows a "no textual change" row; a failed run shows an error row with the exit code. A
  file that is no longer listed produces an info notice.
- Opening `agent`: a viewer with a summary card (id, status, host, category, turns, tokens, cost)
  and the task's recorded output; a missing transcript shows a placeholder row; error lines are
  colored as errors.
- Opening `goal`: the objective wrapped at 48 columns. Opening `memory`: the memory detail rows.
- Viewer popup: about 72 % of terminal height (at least 5 rows, never more than the terminal),
  framed, with a `start-end/total` scroll hint when content overflows. Keys: Esc, `q`, Enter close
  (also the kitty-protocol Esc); Down/`j`, Up/`k` scroll 1; PageDown/PageUp 10; SGR mouse wheel 3.
  Scroll is clamped to the content.
- Command (interface fact: `/side-panel-diff`, registered only when enabled): pick a changed file
  from a list of `<XY> <path>[ +A/-R]` labels (sanitized), then open its diff. Notices instead:
  panel not mounted in this session, no changes, or no exec available. Dismissing the picker does
  nothing.

## 8. Platform notes

- Requires `git` on `PATH` for the files section and diffs; absence just hides them.
- `/dev/null` diff for untracked files works with Git for Windows as well.
- Marker file permissions apply on Unix only.

## 9. Acceptance tests

1. Default config, no flag: no command registered, no reader or timer runs, nothing renders.
2. `enabled=false` + flag on → panel mounts; `enabled=true` + flag off → it does not.
3. Each invalid setting (`width` 23 / 161 / `"9%"` / `"5%"` / `"100%"`, `min_columns` 59,
   `usage_poll_seconds` 3601, unknown key) yields an error naming the field; session continues.
4. Width: `"26%"` at 200 cols → 52; at 80 cols → 32; `200` columns → 80; `200` columns at 100 cols → 40.
5. Below `min_columns` the column is hidden; widening shows it again without remount.
6. Non-fullscreen TUI renders the panel above the editor and shows the notice once across two
   sessions (marker present the second time); unwritable agent home → no notice, no error.
7. Print mode or a context without UI: panel stays dark, no warning.
8. Rows at width 4: `"xy"` → `"xy  "`, `"abcdefg"` → `"abc…"`; width 1 with a long row
   → one cell, no ellipsis; width 0 → no rows.
9. Text containing ESC/BEL/OSC sequences, newlines and bidi overrides renders without control
   bytes; wide CJK characters count 2 cells.
10. Only location present → exactly one row; four non-empty sections → four separator rows.
11. Disabling a section removes it without leaving a blank gap.
12. 13 running tasks are all kept; when one finishes the oldest finished task is evicted to 12.
13. A task finishing twice (repeated update) adds its cost to spend only once.
14. Tasks of another parent session are not listed; with no session id nothing is listed.
15. An interrupted task shows as parked with no duration and is counted in `parked`.
16. 30 tool calls → 24 stored, 6 shown, heading `+18 earlier`; user input clears the list.
17. A staged rename `a.txt` → `b.txt` with staged 4/0 and unstaged 1/2 shows `b.txt` and `+5/-2`;
    an untracked file shows no counts; a binary file shows `+0/-0`.
18. Git refresh twice within 2 s runs git once; a forced refresh runs it again.
19. A running task makes the panel repaint each second; the timer stops once all are terminal.
20. After shutdown, a git result that resolves late does not repaint or change state, and the
    host's original layout and link handler are restored.
21. Link URLs round-trip paths with spaces, `#`, `/` and control bytes, and contain no ESC/BEL.
    Not claimed: a URL of another scheme, an unknown kind, a missing or empty value, a bad
    percent escape, an escape that decodes to invalid UTF-8, and a goal/memory value other than
    `current`.
22. Clicking a file row opens its diff; a diff longer than 5000 lines ends with
    `... N more lines`; a file no longer changed shows an info notice.
23. `/side-panel-diff` with the panel off, with no changes, and with no exec each produce a notice
    and no viewer; dismissing the picker opens nothing.
24. Viewer at 30 rows: 21 rows total, of which 17 are body rows (4 rows of frame/title/hint);
    PageDown then one wheel-down step scrolls to 13; with 50 content rows an oversized scroll is
    clamped to 33; `q` closes, an unbound key is ignored.
25. A goal file with an unknown status or broken JSON hides the goal section; a valid one shows
    status and objective; an unchanged file is not re-parsed.
26. Context usage with a window but no token count shows the placeholder, not `0`.
