# maho-team (MIT migration Phase 3, step 2: team runs and task panes)

| Source (MIT) | Commit | Rust |
|---|---|---|
| can1357/oh-my-pi `packages/coding-agent/src/subprocess/terminal-launch/tmux.ts` | f59bbac | crates/maho-team/src/mux.rs (`TmuxBackend`, `escape_tmux_argument`) |
| oh-my-pi `terminal-launch/cmux.ts`, `terminal-launch/shared.ts`, `terminal-launch/types.ts` | f59bbac | crates/maho-team/src/mux.rs (`CmuxBackend`, `run`) |
| oh-my-pi `packages/tui/src/terminal-multiplexer.ts`, `multiplexers/tmux.ts` | f59bbac | crates/maho-team/src/mux.rs (`detect`) |
| oh-my-pi `packages/tui/test/helpers/fake-tmux.ts` | f59bbac | crates/maho-team/tests/team.rs (`panes_with_fake_tmux`) |
| HazAT/pi-interactive-subagents `pi-extension/subagents/cmux.ts` (tmux split/kill/capture, preference) | c100577 | crates/maho-team/src/mux.rs, src/panes.rs |
| tmustier/pi-agent-teams `extensions/teams/leader-inbox.ts` (completion notices, progress, batch done) | 235fac8 | crates/maho-team/src/team.rs (`Team::step`) |
| JokerQianwei/pi-tmux README/design | 53af731 | (design reference only) |
| clean-room spec docs/cleanroom/specs/team.md | 8737cacc | team model on maho-task, driver, tools, limits, templates, env |

Differences from the originals:

- The pane runs a read-only `tail -F` of the task's log and output; the child itself stays a
  normal `mhc -p` process (originals run the agent inside the pane). Headless and pane runs
  therefore behave the same.
- Members are non-interactive task children: a message to a busy member is queued and delivered
  as a follow-up run when the current run ends (originals steer a live RPC session or a mailbox
  polled by an idle teammate).
- Only tmux and cmux; no shared task list, auto-claim, hooks or styles.
- Team updates are custom `team-update` messages with neutral wording.
