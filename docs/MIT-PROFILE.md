# MIT build profile

`mhc` is built from `maho-cli`. Its cargo feature `omo` (on by default) links the oh-my-openagent
port under `crates/omo/**`, which is licensed under the Sustainable Use License 1.0
(`LicenseRef-SUL-1.0`). Building without default features gives the **MIT profile**: a `mhc` binary
that links no crate under `crates/omo/**`. This is Phase 0 ("isolation") of the MIT migration.
The default build keeps every feature, so nothing changes for it.

## Build

```sh
cargo build -p maho-cli --no-default-features --bins     # mhc, mhc-launcher, mhc-update-probe
cargo build -p maho-senpi-desktop-engine                 # MIT sibling binary (crates/desktop)
```

Do not stage the SUL sibling binaries (`ast-grep-mcp`, `lsp-daemon`, `omo-git-bash`) with an MIT
build. `tools/package-native.mjs` still stages the default product, so it does not package an MIT
build yet.

## What it includes

Everything outside the omo port:

- interactive TUI, print (`-p`), RPC, app-server, multi-session host and the mini/coordinator
  processes;
- every provider, OAuth login and model catalog;
- builtin tools: `read`, `write`, `edit`, `bash`, `grep`, `find`, `ls`, and the rest of `maho-tools`;
- builtin extensions under `crates/builtins` (MCP, hooks, codemode, websearch/webfetch, todo,
  compaction, goal, rules, permission system, ...) and the native user ports under
  `crates/extensions`, except `pi-comment-checker`;
- `crates/desktop/senpi-desktop-*` (MIT; moved out of `crates/omo` so that path checks are exact).

## What is compiled out (needs the `omo` feature)

| Area | In the MIT profile |
| --- | --- |
| omo mount (`<builtin:omo>`): task/subagent tools (`task`, `task_*`, `team_*`, `workflow`/DAG, `workpool`) and `/tasks`, `/task-kill`, `/dag` | not registered |
| memory tools and commands (`/memory`, `/remember`, `/dream`, `/reflect`, `/search`, `/people`, `/palace`, `/init`, `/doctor`, `/facts`, `/memfs`, ...), Kibitzer recall, facts child | not registered |
| LSP tools and `lsp-daemon`; ast-grep MCP; builtin MCPs (context7, grep_app); `x_search`; browser bridge | not registered |
| comment-checker hook (`pi-comment-checker` user port) | not linked |
| model profile, omo config startup/migration/watch, gateway, onboarding, init-deep advisor, git-master, telemetry | not registered |
| bundled omo skills (`OMO_SENPI_SKILLS_ROOT`, the `skills/` root beside the executable) and the packaged root in app-server `skills/list` | not registered; settings and project skill paths still load |
| `builtin:task-member` (`--extension builtin:task-member`) | unknown native extension error |
| Claude Code plugin hooks (`maho-ext-hooks` feature `claude-plugins`) | not discovered; `~/.claude/settings.json` and project `.claude/settings*.json` hooks still load |
| `mhc doctor` | prints "not available in this build", exits 1 |
| `mhc ulw-loop`, `--omo-memory-mcp`, `--memory-supervisor`, `--child-bootstrap` | print "not available in this build", exit 1 |

The gated `maho-cli` modules are marked `#[cfg(feature = "omo")]` in `crates/maho-cli/src/cli/mod.rs`
and `src/main.rs`. They are MIT-labelled files that port the host side of the omo packages, so they
leave the MIT profile along with the SUL crates. Phase 1+ rewrites what should come back (task,
memory, LSP, config) from MIT sources or clean-room specs. OmO-original features (ultrawork,
mass-ulw, persona prompts) are removed, not ported.

## How it is enforced (CI job `mit-profile`)

1. `python3 scripts/check-mit-profile.py`: walks `cargo tree -e normal --target all` for
   `maho-cli --no-default-features` and `maho-senpi-desktop-engine`. It fails if any package
   resolves under `crates/omo/`, carries a license naming SUL, or has no license at all.
2. `cargo deny ... --no-default-features --config deny-mit.toml check licenses`: the `deny.toml`
   allowlist minus `LicenseRef-SUL-1.0`, with workspace crates checked
   (`[licenses.private] ignore = false`).
3. `cargo clippy -p maho-cli --no-default-features --lib --bins -- -D warnings` and a build.
4. `python3 scripts/mit-profile-smoke.py target/debug/mhc`: print-mode chat against a loopback
   model that calls the builtin `write` tool. It checks that `read`/`write`/`edit` and a shell tool (`bash` or the terminal's `bash_*`) are offered,
   that no omo tool is offered, and that `mhc doctor` / `mhc ulw-loop` report they are unavailable.

`tools/license-audit.mjs` also requires every crate under `crates/omo/` to be `LicenseRef-SUL-1.0`
and every crate under `crates/desktop/` to be MIT.

## Known leftovers (not linked SUL code)

- MIT crates still name OmO features in strings and UI: `maho-ext-anthropic-subscription` collapses
  duplicate `<ultrawork-mode>` blocks, `maho-interactive` lights the prompt border for
  `ulw`/`ultrawork` keywords and has ulw tips (hidden without the task command), and the `omo`
  builtin id stays in the extension registry. Clean these up when the features are removed.
- `maho-ext-quiet-model-profile` re-exports the SUL `maho-omo-model-profile`. No binary depends on it,
  and Phase 1 rewrites it.
- Tests and fixtures: the `maho-cli` integration tests for the omo modules are gated on `omo`. The
  rest of the suite runs with default features only.
