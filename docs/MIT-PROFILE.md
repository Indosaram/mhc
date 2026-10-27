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

Package it with `bun tools/package-native.mjs --mit --output <dir> [--binary target/release/mhc]`
(without `--binary` it runs `cargo build --release --locked -p maho-cli --no-default-features --bins`
first). The MIT stage holds `mhc`, `mhc-launcher`, the `mhc-lsp` LSP daemon when it was built beside
`mhc` (or named with `--mhc-lsp-binary`) (and `senpi-desktop-engine` with
`--desktop-engine-binary`), the MIT text and third-party notices, and a manifest with
`profile: "mit"`. It refuses a payload whose `mhc ulw-loop` does not report "not available in this
build", refuses the SUL/OmO staging flags, and `--verify-only` rejects a stage containing
`licenses/SUL-1.0.md`, skills, the omo schema or a SUL sibling (`ast-grep-mcp`, `lsp-daemon`,
`omo-git-bash`). Builtin skills come back with the Phase 4 asset work.

## What it includes

Everything outside the omo port:

- interactive TUI, print (`-p`), RPC, app-server, multi-session host and the mini/coordinator
  processes;
- every provider, OAuth login and model catalog;
- builtin tools: `read`, `write`, `edit`, `bash`, `grep`, `find`, `ls`, and the rest of `maho-tools`;
- builtin extensions under `crates/builtins` (MCP, hooks, codemode, websearch/webfetch, todo,
  compaction, goal, rules, permission system, ...) and the native user ports under
  `crates/extensions` (`pi-comment-checker` uses the MIT `maho-comment-checker` crate here);
- `crates/desktop/senpi-desktop-*` (MIT; moved out of `crates/omo` so that path checks are exact).

## Clean-room replacements (Phase 1)

The MIT profile mounts these from MIT crates written from the specs in `docs/cleanroom/specs`:

- `<builtin:config-notices>` (`maho-config`): runs the startup config migration, loads
  `~/.omo/omo.jsonc` and the project `.omo/omo.jsonc` files, and reports load diagnostics at
  session start;
- `<builtin:config-hot-reload>` (`maho-config`): registers the config watch consumed by
  `config-reload`;
- `<builtin:model-profile>` (`maho-model-select`): applies the configured `model_profile` (or the
  default lanes) to a new session's model;
- `mhc doctor`: a basic report of the config sources (found/loaded), config problems, the active
  config profile, and the model profile resolution against the models available from
  `models.json` + `auth.json` (offline). It exits 1 only for an unknown model profile. Delegated
  category checks return with the Phase 2 task port; models that only an extension provider
  supplies (subscription logins) are not counted.

## MIT ports (Phase 2)

- `<builtin:lsp>` (`maho-lsp`, ported from MIT `pi-lsp-client` @ fc8f612): `lsp_diagnostics`,
  `lsp_goto_definition`, `lsp_find_references`, `lsp_symbols`, `lsp_prepare_rename`, `lsp_rename`,
  the `/lsp` command (`status`, `install <id>`, `warmup <id>`) and post-edit LSP error reporting
  after `write`/`edit`/`apply_patch`. Servers come from the builtin table merged with
  `.omo/lsp-client.json` / `.pi/lsp-client.json` (project) and `<agent dir>/lsp-client.json` /
  `~/.pi/lsp-client.json` (user). When the `mhc-lsp` daemon binary sits beside `mhc` (build it with
  `cargo build -p maho-lsp --bins`), clients link through it so language servers survive across mhc
  processes (unix; `MHC_LSP_DAEMON=0` disables, `MHC_LSP_SOCKET` moves the socket); otherwise
  servers are spawned in-process. The daemon design follows the MIT oh-my-pi LSP mux.
- `pi-comment-checker` user port: links the MIT `maho-comment-checker` crate (hook payload model,
  apply_patch metadata readers, `@code-yeongyu/comment-checker` binary lookup with
  `COMMENT_CHECKER_BINARY`/`PATH` fallbacks).
- Formatter / post-mutation hooks: interfaces in `maho_lsp::hooks` (`Formatter`,
  `PostMutationHook`, `HookChain`); the LSP diagnostics hook is the first implementation.

## What is compiled out (needs the `omo` feature)

| Area | In the MIT profile |
| --- | --- |
| omo mount (`<builtin:omo>`): task/subagent tools (`task`, `task_*`, `team_*`, `workflow`/DAG, `workpool`) and `/tasks`, `/task-kill`, `/dag` | not registered |
| memory tools and commands (`/memory`, `/remember`, `/dream`, `/reflect`, `/search`, `/people`, `/palace`, `/init`, `/doctor`, `/facts`, `/memfs`, ...), Kibitzer recall, facts child | not registered |
| ast-grep MCP; builtin MCPs (context7, grep_app); `x_search`; browser bridge | not registered |
| gateway, onboarding, init-deep advisor, git-master, telemetry | not registered |
| bundled omo skills (`OMO_SENPI_SKILLS_ROOT`, the `skills/` root beside the executable) and the packaged root in app-server `skills/list` | not registered; settings and project skill paths still load |
| `builtin:task-member` (`--extension builtin:task-member`) | unknown native extension error |
| Claude Code plugin hooks (`maho-ext-hooks` feature `claude-plugins`) | not discovered; `~/.claude/settings.json` and project `.claude/settings*.json` hooks still load |
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
   that no omo tool is offered, that `mhc doctor` reports the config sources and resolves a pinned
   model profile, and that `mhc ulw-loop` reports it is unavailable.
5. `cargo nextest run -p maho-cli --no-default-features`: the `maho-cli` tests in the MIT profile.

The `license-audit` job also runs `bun tools/package-native.mjs --self-test`, which covers the
`--mit` staging.

`tools/license-audit.mjs` also requires every crate under `crates/omo/` to be `LicenseRef-SUL-1.0`
and every crate under `crates/desktop/` to be MIT.

## Known leftovers (not linked SUL code)

- The directive dedupe in `maho-ext-anthropic-subscription` and the prompt-border glow in
  `maho-interactive` are neutral: they act only on tags/keywords registered at startup, and only the
  default (`omo`) build registers `<ultrawork-mode>` and `ulw`/`ultrawork` (`maho-cli` main.rs).
  Still left: ulw tips in `maho-interactive` (hidden without the task command), the `omo` builtin
  id in the extension registry, and the `ultrawork`/`ulw-loop` wording in a `maho-ext-prompt-preset`
  directive and `senpi_install.rs`.
- `maho-ext-quiet-model-profile` (a re-export of the SUL `maho-omo-model-profile` with no consumer)
  was removed from the workspace; `maho-model-select` replaces it in the MIT profile.
- Tests and fixtures: the `maho-cli` integration tests for the omo modules are gated on `omo`. The
  rest of the suite runs with default features only.
