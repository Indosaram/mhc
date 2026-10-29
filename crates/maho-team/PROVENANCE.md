# Provenance — maho-team

- License: MIT.
- Method: **P** (port of MIT originals) for multiplexer detection, tmux/cmux pane launch and close,
  the fake-CLI test approach and the team notification shapes; **C** (clean-room, from
  `docs/cleanroom/specs/team.md`, commit 8737cacc) for the parts without an original: teams
  mapped onto non-interactive `maho-task` children (members are tasks, messages are follow-up
  runs queued while a run is active), the driver, the `task.team` limits, `teams.<name>`
  templates, the `team_*` tool surface and the `MAHO_TEAM_*` variables.
- Origins (license verified as MIT before reading):
  - `can1357/oh-my-pi` @ `f59bbac3d56e8b485fef466388b1cb5be4b93123` (LICENSE MIT, Copyright 2025
    Mario Zechner, 2025-2026 Can Bölük, 2026 Stencil Labs, Inc.):
    `packages/coding-agent/src/subprocess/terminal-launch/tmux.ts` (`split-window`/`new-window`,
    `-c` with `#` doubled, trailing-semicolon escaping, `-P -F` id reply and validation,
    multi-argument direct execution) → `src/mux.rs` `TmuxBackend`;
    `terminal-launch/cmux.ts` (`--json new-split`, `data` envelope, `pane_id`/`pane_ref` ids) →
    `src/mux.rs` `CmuxBackend`; `terminal-launch/{shared,types}.ts` (sanitized errors that omit
    argv/output) → `src/mux.rs` `run`; `packages/tui/src/{terminal-multiplexer.ts,
    multiplexers/tmux.ts}` (`$TMUX` session marker) → `src/mux.rs` `detect`;
    `packages/tui/test/helpers/fake-tmux.ts` (fake CLI on PATH) → `tests/team.rs`.
  - `HazAT/pi-interactive-subagents` @ `c100577ebf7393a11d098ad9810ec6c269dcfc30` (LICENSE MIT,
    Copyright 2026 HazAT): `pi-extension/subagents/cmux.ts` (backend preference variable,
    "inside tmux = `$TMUX` and `tmux` on PATH", split off `$TMUX_PANE` with `-d` so focus stays,
    `kill-pane`, `capture-pane` read-back used by the tests) → `src/mux.rs`, `src/panes.rs`.
  - `tmustier/pi-agent-teams` @ `235fac8622faed4b0f048ca285237189f590bc5a` (LICENSE MIT,
    Copyright 2026 Thomas Mustier): `extensions/teams/leader-inbox.ts` (per-member completion
    notice with result preview and `done/total` progress, batch "all done" notice, follow-up
    delivery), `README.md` (team lifecycle: spawn, direct message, broadcast, stop/done) →
    `src/team.rs` (texts rewritten).
  - `JokerQianwei/pi-tmux` @ `53af731148c4cd5f5132a7f02900ecbb5817fbb4` (LICENSE MIT, Copyright
    2026 JokerQianwei): README/design only (visible named children the parent waits on).
- Not ported: zellij/WezTerm/herdr/orca backends, interactive teammates with RPC mailboxes, a
  shared task list with dependencies and auto-claim, styles, hooks/quality gates, session
  branching, the TUI widget.
- No SUL source (`crates/omo/**`, in particular `team-core`, `tmux-core`, `senpi-task`; the
  omo-gated `maho-cli` modules; OmO/COMP sources) was opened.
- Tests were written from the spec (fake runner, fake `tmux` script, a real tmux server when one
  is on PATH); no upstream or OmO fixtures were reused.

## Task control and team tasklist (Phase 4)

- Method: **C** (clean room).
- Spec: `docs/cleanroom/specs/task-control.md` at spec commit `8f3d2e29` (plus the files it
  references: `task.md`, `team.md`, `gateway.md`).
- Allowed inputs of the implementing session: the spec files under `docs/cleanroom/specs/`,
  `AGENTS.md`, and the MIT code already in this checkout (`crates/maho-task`, `crates/maho-team`,
  `crates/maho-gateway`, `crates/maho-ext-api`, `crates/maho-tools`, `crates/maho-config`,
  `crates/maho-cli/src/cli/{delegation,team_view,session_gateway,default_extensions}.rs`).
- No forbidden input was read: nothing under `crates/omo/**`, `assets/omo-latest/**`,
  `assets/omo.schema.json` or `.omo/**`, no oh-my-openagent / OmO / lazycodex source or URL, no
  git history of removed paths, and no other machine's copy of this repository.
- Tests were written new from the spec's numbered acceptance tests (§6); no fixtures were reused.
