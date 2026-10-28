# Phase 4-5 inventory: SUL-1.0 crates and their MIT disposition

Source: `crates/omo/**` at `6957a9a8` (58 crates). Each crate gets one disposition:

- **covered**: an MIT crate already provides the general behavior (spec listed).
- **spec**: a general feature with no MIT equivalent; Phase 4 writes a clean-room spec and rebuilds it.
- **dropped**: OmO-original product feature; removed without replacement.
- **deferred**: kept out of this migration by an earlier user decision.

LOC = lines of `.rs` in the crate (tracked files).

## Crates

| Crate (path under crates/omo) | LOC | Disposition | MIT home / spec unit |
|---|---:|---|---|
| ast-grep-mcp | 6891 | spec | `ast-grep` |
| boulder-state | 5777 | dropped | start-work/boulder |
| claude-code-compat-core | 5023 | spec | `claude-plugin-hooks` |
| comment-checker-core | 2369 | covered | maho-comment-checker |
| components/maho-omo-agent-home | 625 | covered | maho-agent-home (`agent-home.md`) |
| components/maho-omo-ast-grep | 227 | spec | `ast-grep` |
| components/maho-omo-browser-bridge | 566 | covered | maho-browser (`browser-tools.md`) |
| components/maho-omo-builtin-mcps | 769 | covered | maho-mcp-builtins (`mcp.md`) |
| components/maho-omo-bundled-skills | 813 | covered | maho-skills (`skills.md`) |
| components/maho-omo-claim-receipt | 5416 | dropped | ulw evidence receipts |
| components/maho-omo-comment-checker | 1944 | covered | maho-comment-checker |
| components/maho-omo-computer-use | 5619 | deferred | engine is MIT in `crates/desktop`; tool registration deferred since 2026-10-09 |
| components/maho-omo-config-resolution | 90 | covered | maho-config (`config.md`) |
| components/maho-omo-config-startup | 1247 | covered | maho-config (`config.md`) |
| components/maho-omo-config-watch | 1190 | covered | maho-config (`config.md`, reload) |
| components/maho-omo-desktop-toolkit | 411 | deferred | with computer-use |
| components/maho-omo-fallback-architect | 641 | dropped | persona routing |
| components/maho-omo-feedback | 2636 | spec | `feedback` |
| components/maho-omo-formatter | 1239 | spec | `post-edit-format` |
| components/maho-omo-gateway | 14305 | covered | maho-gateway (`gateway.md`) |
| components/maho-omo-git-master | 1081 | covered | maho-git |
| components/maho-omo-init-deep-advisor | 984 | covered | `init-deep.md` skill |
| components/maho-omo-lsp | 1860 | covered | maho-lsp |
| components/maho-omo-mass-ulw | 218 | dropped | mass-ulw |
| components/maho-omo-memory | 68209 | covered | maho-memory (`memory-format.md`) |
| components/maho-omo-model-profile | 5433 | covered | maho-model-select (`model-selection.md`) |
| components/maho-omo-onboarding | 13294 | covered | maho-onboarding (`onboarding.md`) |
| components/maho-omo-post-mutation | 484 | spec | `post-edit-format` |
| components/maho-omo-question-gates | 310 | spec | `question-gates` |
| components/maho-omo-side-panel | 4748 | spec | `side-panel` |
| components/maho-omo-skill-commands | 1568 | covered | maho-skills (`skills.md`) |
| components/maho-omo-skill-pointers | 1573 | covered | maho-skills (`skills.md`) |
| components/maho-omo-start-work-continuation | 1427 | dropped | start-work/boulder |
| components/maho-omo-task | 26020 | covered + spec | maho-task (`task.md`); gaps -> `workflow-dag`, `workpool`, `task-control` |
| components/maho-omo-telemetry | 14996 | covered | maho-telemetry (`telemetry.md`) |
| components/maho-omo-todo-fanout-reminder | 206 | dropped | ulw nudge |
| components/maho-omo-ultrawork | 517 | dropped | ultrawork |
| components/maho-omo-ulw-loop | 2110 | dropped | ulw-loop |
| components/maho-omo-x-search | 4213 | spec | `x-search` |
| config-migration | 4368 | covered | maho-config (`config-keys.md`) |
| delegate-core | 893 | covered | maho-task (`task.md`) |
| get-worker | 3457 | covered | maho-task (`task.md`) |
| git-bash-mcp | 1825 | spec | `git-bash-mcp` |
| isolation-core | 11786 | covered | maho-isolation (`isolation.md`) |
| lsp-core | 18215 | covered | maho-lsp |
| lsp-daemon | 8171 | covered | maho-lsp |
| maho-omo | 7130 | dropped | composition root; MIT wiring is `default_extensions.rs` |
| mcp-stdio-core | 2285 | covered | maho-mcp-builtins (`mcp.md`) |
| memory-core | 87866 | covered | maho-memory (`memory-format.md`) |
| model-core | 14519 | covered | maho-model-select (`model-selection.md`) |
| omo-config-core | 18896 | covered | maho-config (`config.md`, `config-keys.md`) |
| prompts-core | 623 | dropped | persona prompts |
| senpi-task | 137990 | covered + spec | maho-task; gaps -> `workflow-dag`, `workpool`, `task-control` |
| team-core | 10875 | covered + spec | maho-team (`team.md`); gap -> `task-control` (team tasklist) |
| telemetry-core | 12036 | covered | maho-telemetry (`telemetry.md`) |
| tmux-core | 3820 | spec | `tmux-panes` |
| ulw-loop-core | 15163 | dropped | ulw-loop |
| utils | 26393 | covered | maho-support (`utils.md`) |

Tally (58 crates): covered 31, covered + spec 3, spec 11, dropped 11, deferred 2.

## Non-crate items

| Item | Disposition |
|---|---|
| `maho-cli` `omo` feature and every `cfg(feature = "omo")` block | delete; keep `not(omo)` branches unconditionally |
| maho-cli modules only built with `omo` (doctor, facts_child, host_transport, kernel_tools_host, kibitzer_*, memory_runtime, omo_mount, omo_shipped, recall_wiring, task_host, task_runners, task_session) | delete |
| CLI: `ulw-loop`, `schedule`, `import-omo`, `senpi-install`, `host`, `omo doctor`, `--memory-supervisor`, `--child-bootstrap` | delete |
| `tools/check-omo-sites.mjs`, `tools/latest-omo-*.mjs`, `tools/omo-schema.mjs`, `tools/omo-sites.json` | delete with their CI steps |
| `assets/omo-latest` (26 MIT skills) | keep; rename to a neutral path in G4; drop the ulw/ultrawork skills that only drive dropped features |
| `LICENSES/SUL-1.0.md`, SUL text in LICENSE/README/CONTRIBUTING/AGENTS, `LicenseRef-SUL-1.0` in about/deny | delete |

## New spec units (Phase 4)

Only behavior with no existing spec in `docs/cleanroom/specs/`:

1. `workflow-dag`: staged multi-agent workflow runner (nodes, dependencies, fan-out/fan-in, per-node agent spec, retry), exposed as a tool and `/dag`.
2. `workpool`: keyed work queue with durable item ids, push/close/inspect/cancel, aggregate delivery on close.
3. `task-control`: send a message to a running child, cancel/kill (`/task-kill`), and the team tasklist (create/get/list/update with claim transitions).
4. `ast-grep`: structural search/rewrite tools (pattern + rewrite, dry run default) backed by the `ast-grep` binary.
5. `claude-plugin-hooks`: load Claude Code plugin hook definitions and run them on tool/session events.
6. `post-edit-format`: run the configured formatter on files changed by edit/write tools.
7. `question-gates`: structured ask-user question tool with wait/no-wait modes.
8. `feedback`: `/feedback` and `/bug` commands that assemble a report bundle.
9. `side-panel`: secondary TUI panel for background task/status views.
10. `x-search`: X/Twitter search tool via the xAI API.
11. `git-bash-mcp`: Windows Git Bash command runner exposed as a stdio MCP server.
12. `tmux-panes`: open team/child sessions in tmux panes when running inside tmux.

## Dropped (OmO-original, no replacement)

ultrawork, ulw-loop (product + core), mass-ulw, start-work/boulder (+ continuation), claim receipts, todo-fanout reminder, fallback-architect persona routing, persona prompts (`prompts-core`), kibitzer, the `maho-omo` composition root.

## Phase 5 removal checklist

- [ ] `crates/omo/**` deleted; workspace members/deps updated
- [ ] `omo` feature and all `cfg(feature = "omo")` removed; omo-only modules and subcommands deleted
- [ ] omo tools and CI steps removed; `scripts/ci-shard.py` covers every remaining member once
- [ ] `LICENSES/SUL-1.0.md` removed; LICENSE, README(.en), CONTRIBUTING, AGENTS MIT-only
- [ ] `about.toml`/`deny*.toml` no longer allow `LicenseRef-SUL-1.0`; deny-mit covers the whole workspace
- [ ] `assets/omo-latest` moved to a neutral skills path; package-native and release workflows updated
- [ ] cargo check + clippy `-D warnings` green on Linux, Windows, macOS
