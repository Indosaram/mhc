# Terminal panes for delegated work — behavior spec (Phase 4, unit 12)

Scope: everything the MIT profile needs to show team members and other delegated tasks in tmux
panes beyond what [team.md](team.md) §4 already fixes. team.md owns the pane *policy* (which
tasks get a pane, the follower command, linger, the `MAHO_TEAM_*` variables). This spec owns the
*multiplexer layer* underneath it: running tmux commands, detection details, placement modes
(split / window / separate session), window layout and main-pane sizing, graceful close, and
cleanup of panes and sessions left behind by a crashed lead. It replaces `tmux-core`
(`crates/omo/tmux-core`, SUL), which is deleted in Phase 5.

Implement it in `crates/maho-team` next to the existing `TmuxBackend` (team.md §7), or as a small
`maho-mux` module that crate uses. Proposed neutral names: `MuxRunner`, `MuxEnv`, `Placement`,
`WindowLayout`, `StaleSweep`. Implementers may pick other neutral names.

Behaviour of the SUL crate that is **not** carried over: attaching panes to a remote agent server
(health probing of an HTTP endpoint, `attach` commands, password/username environment
propagation into panes, placeholder panes that are later respawned into an attached client). In
mhc the pane only follows the child's files (team.md §4.3), so none of that applies.

## 1. Running a multiplexer command

- A command is `tmux <args...>` with the arguments passed as separate argv entries (never through
  a shell). stdin is null; stdout and stderr are captured and trimmed of surrounding whitespace.
- Result: exit code, stdout, stderr. A spawn failure (no executable, permission denied) is
  reported as exit code 1 with the OS error text in stderr; it never panics or aborts the task.
- Optional timeout: when it elapses, the process (and its children) is killed and the result is
  exit code -1 with stderr `timeout`.
- Optional retry count N: a failing command is run again up to N more times, **except** when
  stderr says the target is gone (case-insensitive `can't find pane` or `can't find session`).
  Those failures are final at once.
- cmux shim (§2.2): when the cmux-through-tmux environment is detected, the same arguments are
  run as `cmux __tmux-compat <args...>`. If the configured tmux path already names a `cmux`
  executable (`cmux`, or `cmux` plus `.exe`, `.cmd`, `.bat` or `.ps1`, case-insensitive), that path
  is used instead of a bare `cmux`.

## 2. Detection

### 2.1 Inside tmux

"Inside tmux" means the `TMUX` variable is set and non-empty. An empty `TMUX` counts as unset.
The lead's own pane is `$TMUX_PANE` when set. The tmux executable is looked up on `PATH` once per
process; when it is missing, every operation in this spec is a no-op that reports "unavailable"
and issues no command.

### 2.2 cmux through the tmux shim

The environment is "cmux via tmux shim" when either:

- `TMUX` contains the substring `cmuxterm`; or
- `CMUX_SOCKET_PATH` is set and non-empty while `TMUX` is unset or empty.

In that case tmux-style operations go through §1's shim. A "native tmux" environment is inside
tmux and not cmux-via-shim. "Pane capable" = inside tmux or cmux-via-shim. The native cmux CLI
backend of team.md §4.1 (detected by `CMUX_WORKSPACE_ID`/`CMUX_SURFACE_ID`) is still preferred
when available. The shim is the fallback for cmux setups that only expose the tmux interface.

## 3. Placement

`MAHO_TEAM_PANE_LAYOUT` (team.md §5) gains a third value:

| Value | Where the follower pane goes |
|---|---|
| `split` (default) | split off the lead's pane, as team.md §4.3 |
| `window` | a new detached window in the lead's tmux session, as team.md §4.3 |
| `session` | a separate detached tmux session owned by this lead process (§3.1) |

All placements run tmux detached (`-d`), so focus stays with the lead. Each also asks tmux to
print the new id (`-P -F '#{pane_id}'`) and treats an empty reply or a non-zero exit as an open
failure. Per team.md §4.3, an open failure leaves the task headless and records the error.

### 3.1 Separate session

- Session name: `mhc-tasks-<lead pid>`, or `mhc-tasks-<lead pid>-<tag>` when a lead runs more than
  one pane group (tag: ASCII letters and digits only).
- If `has-session -t <name>` succeeds, the pane is opened with `new-window -t <name>`. Otherwise it
  is opened with `new-session -d -s <name>`. When the lead's pane is known, the new session copies
  the lead window's size: `display -p -t $TMUX_PANE '#{window_width},#{window_height}'` is read and
  passed as `-x <w> -y <h>`. If that query fails or does not parse, the size flags are left out.
- Available only in native tmux (not through the cmux shim). Elsewhere `session` falls back to
  `split`.
- The user reaches the panes with `tmux switch-client -t mhc-tasks-<pid>`. The `/team` listing
  (team.md) shows the session name.

### 3.2 Titles

After a successful open, the pane title is set with `select-pane -t <id> -T <title>`. The title
comes from team.md §4.3, with any NUL characters removed, cut to 40 characters, and every `#`
doubled. A failing title command is logged and ignored.

## 4. Window layout (split placement)

Optional variables:

| Variable | Values | Default |
|---|---|---|
| `MAHO_TEAM_PANE_ARRANGE` | `main-vertical`, `main-horizontal`, `tiled`, `even-horizontal`, `even-vertical`, `none` | `none` |
| `MAHO_TEAM_PANE_MAIN_SIZE` | integer percent | `50` |
| `MAHO_TEAM_PANE_MAIN_MIN` | columns | `0` |
| `MAHO_TEAM_PANE_SIDE_MIN` | columns | `0` |

The layout names are tmux's own layout names (interface facts). Invalid values are ignored with
one warning, and the default applies.

- `none`: panes keep the shape that the splits of team.md §4.3 give them.
- Otherwise, after every open and every close in the lead's window: `select-layout <arrange>`.
  Then, for `main-vertical`, `set-window-option main-pane-width <size>%`; for `main-horizontal`,
  `set-window-option main-pane-height <size>%`. Other layouts set no main-pane option.
- For `main-vertical`, the lead pane is then resized to an exact column width:
  `display -p -t <lead> '#{pane_width},#{window_width}'` gives the window width W, and the
  lead gets `resize-pane -t <lead> -x <M>` where
  - p = size clamped to 20..80;
  - M = floor((W - 1) * p / 100), raised to at least `MAIN_MIN`, then lowered to at most
    W - 1 - `SIDE_MIN` (never below 0).
- Layout and resize failures are logged and ignored. They never fail the open or the task.

## 5. Closing

Closing replaces team.md §4.4's plain kill (after the linger):

1. Send `send-keys -t <pane> C-c` so the follower can exit cleanly.
2. Wait 250 ms.
3. `kill-pane -t <pane>` (`kill-window -t <window>` for window placement).
4. The close **succeeds** when the kill succeeds, or when it fails with `can't find pane` (the
   pane already exited after step 1, or the user closed it). Any other failure is logged; the
   result is "not closed", and nothing is retried.

With separate-session placement, closing the last pane leaves tmux to end the session. When the
lead process exits normally, it kills its own session: `has-session` first, then `kill-session`
only if the session exists. Not inside tmux, or tmux missing → no command, result false.

## 6. Stale cleanup

A lead that crashed leaves `mhc-tasks-*` sessions behind. When the first pane of a process is
about to open, and only when inside native tmux:

1. `list-sessions -F '#{session_name}'`; a failing list yields no candidates (logged).
2. A candidate matches only if its whole name is `mhc-tasks-<digits>` or
   `mhc-tasks-<digits>-<ASCII alphanumerics>`. Every other name, including foreign sessions and
   look-alikes such as `mhc-tasks-12x` or `mhc-tasks-12-`, is never touched.
3. The session is killed (via §5's has-session/kill-session) when its pid is not the current
   process and no process with that pid is alive.
4. A failing kill is logged and the sweep continues. The sweep reports the number of sessions it
   killed.

The follower panes of team.md §4.3 already end when the lead pid dies. The sweep removes the
empty sessions and windows that remain.

## 7. Platform notes

- Unix only (tmux). On Windows, detection always says "not inside tmux" unless a cmux
  shim environment is present; pane modes then degrade to headless exactly as team.md.
- Process liveness (§6) uses signal 0 on Unix.
- No command ever goes through a shell. The only shell text is the follower command of team.md
  §4.3, which tmux receives as a single argument after `--`.

## 8. Acceptance tests

Use a fake `tmux` (and fake `cmux`) script on `PATH` that records its argv and replies from a
script table, plus a controlled environment map. Do not mutate the test process's environment.

1. Runner: exit 0 → trimmed stdout returned; exit 1 → stderr captured; missing executable →
   exit 1 with the OS error, no panic.
2. Runner retry 2 on a generic failure runs 3 times; on `can't find pane` or `can't find session`
   it runs once.
3. Runner timeout 50 ms against a fake that sleeps returns exit -1 / `timeout`, and the fake's
   process is gone afterwards.
4. Detection: `TMUX` set → inside; `TMUX=""` → not inside; `TMUX` containing `cmuxterm` → shim;
   `CMUX_SOCKET_PATH` only → shim and pane capable but not inside tmux; normal `TMUX` → native.
5. Shim: the same operation records `cmux __tmux-compat <args>`. With a tmux path ending in
   `cmux.exe`, that path is the program.
6. Placement `split` records `split-window -d ... -P -F #{pane_id}` against `$TMUX_PANE`.
   Placement `window` records `new-window -d`. An empty id reply → open failure, task headless.
7. Placement `session`: no existing session → `new-session -d -s mhc-tasks-<pid> -x W -y H`
   (from the scripted `display` reply). Existing session → `new-window -t mhc-tasks-<pid>`. A
   failing `display` → no `-x/-y`. Under the cmux shim → falls back to split.
8. Title: NULs stripped, `#` doubled, cut to 40 characters. A failing `select-pane -T` does not
   fail the open.
9. Layout: `main-vertical` with size 60 records `select-layout main-vertical` and
   `set-window-option main-pane-width 60%`; `main-horizontal` records `main-pane-height`;
   `tiled` records no main-pane option; `none` records nothing.
10. Main width: W=101, p=29 → 28; W=100, p=95 → 79 (clamped to 80%); W=100, p=50, MAIN_MIN=90,
    SIDE_MIN=20 → 79; W=0 → 0. When tmux is missing, no resize is issued.
11. Close order is `send-keys C-c`, then `kill-pane`. Kill failing with `can't find pane` →
    success. Kill failing with another error → not closed. Not inside tmux / no tmux → false
    with no commands.
12. Session kill: `has-session` failing → no `kill-session`, false; otherwise
    `kill-session`, true.
13. Sweep: given candidates `mhc-tasks-<dead>`, `mhc-tasks-<dead>-a1`, `mhc-tasks-<self>`,
    `mhc-tasks-<alive>`, `mhc-tasks-12x`, `other`, only the two dead ones are killed (count 2).
    A kill returning false is not counted; a failing list → 0; not inside tmux → 0 and no list.
14. With a real tmux server (skipped when tmux is absent), run in a detached test session: open
    a split follower pane, apply `main-vertical`, close it gracefully, and confirm `list-panes`
    no longer shows it. Then run a sweep against a leftover `mhc-tasks-<dead pid>` session and
    confirm it is gone.

## Leak self-check

- [x] no fenced code block from the SUL tree; the only code-like text is tmux argv and
      environment-variable names (interface facts of tmux, cmux and team.md);
- [x] no sentence translates or paraphrases a SUL comment, doc comment or prompt; the SUL
      placeholder banner and log texts are not reproduced;
- [x] the layout list is tmux's own set of layout names (interface fact), not a mirrored constant
      table; the SUL's polling/grace constants are not carried over;
- [x] identifiers are neutral (`mhc-tasks-<pid>`, `MAHO_TEAM_PANE_*`, `MuxRunner`, `Placement`);
      the SUL session/window/title names are replaced;
- [x] no OmO-original feature; the remote-server attach and auth-propagation behaviour is
      explicitly excluded.
