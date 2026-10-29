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
`omo-git-bash`). The skill loader supports a bundled `skills/` root beside the binary (or
`MAHO_SKILLS_ROOT`), but staging the MIT skill set there is Phase 4 asset work.

## What it includes

Everything outside the omo port:

- interactive TUI, print (`-p`), RPC, multi-session host and the mini/coordinator
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
- `<builtin:task>` (`maho-task`, task model ported from MIT oh-my-pi @ f59bbac): subagent
  delegation tools `task` (one task or a `tasks` batch with a shared `context`; `agent`,
  `category`, `model`, `name`, `background`), `task_output` (result or status, optional bounded
  wait), `task_list`, `task_resume` (follow-up in the task's own session) and `task_cancel`, plus
  `/tasks`. Each subagent runs as a child `mhc -p` process of the same executable with its own
  session under `<agent dir>/tasks/<session id>/` (or `task.state_dir`), so it has the same tools,
  extensions and logins. Background results are delivered to the session as a follow-up message.
  Agents: bundled `task` (general worker) and `scout` (read-only research), `*.md` agents in the
  project `.omo/agents/` and `<agent dir>/agents/`, and the `agents` config section. Models:
  explicit `model`, then a configured `categories.<name>` (no built-in categories), then the
  agent's models, resolved by `maho-model-select` with the parent session's model as the system
  default. Only the main session delegates by default (`task.max_depth` = 1); concurrency follows
  `task.default_concurrency`. Isolated tasks (`isolated: true`, default `task.isolation.enabled`)
  run through `maho-isolation` and team runs / tmux panes through `maho-team` (Phase 3, below).
  The `task` tool also accepts `pane`. Not included: `workflow`/DAG and `workpool` tools,
  `/task-kill`, `/dag`.
- `<builtin:memory>` (`maho-memory`, ported from the MIT mnemopi engine in oh-my-pi @ f59bbac):
  long-term memory in SQLite banks under `$MNEMOPI_DATA_DIR`, else `<agent dir>/memories/mnemopi`
  (`mnemopi.db` is the global bank, `banks/<name>/mnemopi.db` the others). Tools `retain` (one or
  more facts; `scope: global` writes the global bank, else this project's bank), `recall`
  (lexical hybrid recall: FTS5 rank, keyword/word-form relevance, synonyms, importance, recency,
  trust weights), `memory_get` and `memory_forget`; a memory section in the system prompt; on the
  first prompt of a session the best matches are recalled into a hidden `<memories>` message; and
  `/memory` (`status`, `search <query>`, `import-legacy [--dry-run] [--include-people]`). Sessions
  write to a per-project bank and also recall the global bank (`MAHO_MEMORY_SCOPING` =
  `per-project-tagged` (default) | `per-project` | `global`). `memory.enabled: false` in the omo
  config turns it off. Shell: `mhc memory status|search|remember [--global]|import-legacy|mcp`.
  The memory MCP server (`mhc memory mcp`, `mhc --memory-mcp`, and the legacy `--omo-memory-mcp`
  entry) serves the `mnemopi_*` tools (remember, recall, get, update, forget, invalidate, stats,
  scratchpad, triples, export/import) over stdio; `--bank`, `MNEMOPI_MCP_BANK` and
  `MNEMOPI_SESSION_ID` select the bank and session. Not included: embeddings, LLM extraction or
  consolidation (`sleep`/dream/reflect), automatic retain of finished turns, people profiles,
  git-backed Markdown memory and the omo memory commands.
- Legacy memory import: `mhc memory import-legacy [--from <dir>] [--dry-run] [--include-people]
  [--json]` (or `/memory import-legacy`) reads the legacy Markdown store (`$MAHO_MEMORY_HOME`, else
  `~/.omo/memory`) read-only and writes each Markdown block as an imported memory into the global
  bank; re-running skips what is already there, and `people/` files are skipped unless
  `memory.people.enabled` is true or `--include-people` is given. Format and mapping:
  `docs/cleanroom/specs/memory-format.md`.
- `<builtin:browser>` (`maho-browser`, ported from the MIT omowright @ a019fdf, pi-cua-integration @
  9ba0c9e and oh-my-pi @ f59bbac; tool behaviour in `docs/cleanroom/specs/browser-tools.md`):
  - `browser`: drives Chromium over the DevTools protocol. Actions `open`, `goto`, `back`,
    `forward`, `reload`, `snapshot` (accessibility tree with `[ref=eN]` element refs), `click`,
    `fill`, `type`, `press`, `select`, `hover`, `scroll`, `screenshot` (image block, optional
    `path`), `evaluate`, `wait`, `tabs`, `close`; tabs are named (default `main`). The browser is
    launched on first use with a temporary profile and closed at session shutdown. Executable:
    `app.path`, else `$MAHO_BROWSER_PATH` / `$PUPPETEER_EXECUTABLE_PATH` / `$CHROME_PATH`, else the
    first system Chrome/Chromium/Edge. Headless unless `headed: true` or `$MAHO_BROWSER_HEADLESS=0`.
    Transport: `--remote-debugging-pipe` on Unix (no listening port), `--remote-debugging-port=0`
    on Windows or with `$MAHO_BROWSER_TRANSPORT=port`; `app.cdp_url` or `$MAHO_BROWSER_CDP_URL`
    attaches to a running browser instead.
  - `computer`: screenshot + coordinate actions (`click`, `double_click`, `move`, `drag`,
    `scroll`, `type`, `key`) on the viewport of a browser tab (`backend: "browser"`, the default
    when a tab is open) or the host desktop / a Cua sandbox (`backend: "desktop"`: the vendored
    pi-cua-integration daemon, written to `<agent dir>/computer-use/` and run with
    `$MAHO_COMPUTER_PYTHON`, default `python3`; needs Python 3.12/3.13 with `pip install cua`;
    `status`, `sandbox_start|stop|list`, cloud sandboxes read `CUA_API_KEY`). Without `cua` the
    desktop actions fail with the daemon's "cua Python package is not installed" error.
  - Not ported: oh-my-pi's JavaScript-execution browser runtime, relay/Tern/cmux browsers, stealth
    patches and Chrome for Testing download; omowright's network capture, frames, HAR and tracing.
    The default (`omo`) build does not mount these tools (its browser bridge and computer-use
    crates were never mounted).
- `<builtin:telemetry>` (`maho-telemetry`, ported from the MIT oh-my-pi @ f59bbac telemetry export
  and stats packages; opt-outs, ledger and `mhc usage` in `docs/cleanroom/specs/telemetry.md`):
  - Local usage ledger: one JSON line per model call (provider, model, token counters, recorded
    cost, duration, time to first token, stop reason, session id, cwd; never prompt, response or
    tool text) appended to `<agent dir>/stats/usage.jsonl` (`$MAHO_USAGE_DIR` overrides the
    directory). It works for `--no-session` runs and never leaves the machine.
  - `mhc usage [--json] [--sessions] [--days N] [--file PATH] [--status]` and `/usage`: requests,
    errors, tokens, cache rate and savings, API-equivalent cost estimate, latency, by model and by
    folder (`--sessions` reads the session logs instead of the ledger; `--status` shows what is on).
  - OTLP export (opt-in): only when an `OTEL_EXPORTER_OTLP_ENDPOINT` or per-signal
    `OTEL_EXPORTER_OTLP_{TRACES,METRICS,LOGS}_ENDPOINT` is set. Each agent run is pushed as
    OTLP/HTTP protobuf: GenAI spans (`invoke_agent mhc`, `chat <model>`, `execute_tool <tool>`),
    `gen_ai.client.token.usage` and `mhc.agent.*` metrics, and an `agent run completed` log record.
    `OTEL_*_EXPORTER=none`, `OTEL_EXPORTER_OTLP_*HEADERS`, `OTEL_SERVICE_NAME`,
    `OTEL_RESOURCE_ATTRIBUTES` and `OTEL_LOG_LEVEL` are honoured; only `http/protobuf`. There is no
    built-in endpoint, vendor or installation id.
  - Opt-outs: `MAHO_TELEMETRY=0` or config `telemetry.enabled: false` (everything);
    `DO_NOT_TRACK=1`, `OTEL_SDK_DISABLED=true` or `telemetry.otlpExportEnabled: false` (export);
    `MAHO_USAGE_STATS=0` or `telemetry.usageStats: false` (ledger).
  - The default (`omo`) build links the crate but does not mount it.
- Formatter / post-mutation hooks: interfaces in `maho_lsp::hooks` (`Formatter`,
  `PostMutationHook`, `HookChain`); the LSP diagnostics hook is the first implementation.

## MIT ports and clean-room work (Phase 3)

- Task workspace isolation (`maho-isolation`, ported from the MIT oh-my-pi @ f59bbac `pi-iso`
  rcopy backend and task isolation runner; non-git parts from `docs/cleanroom/specs/isolation.md`),
  the backend behind `<builtin:task>`:
  - An isolated child runs in its own directory `<root>/t<hash>/m`, root `$MAHO_WORKTREE_DIR`
    (absolute) or `<agent dir>/wt`. Inside git it is a detached `git worktree` of the repository
    that mirrors the parent's staged, unstaged and untracked (non-ignored) state; outside git (or
    with `task.isolation.backend: copy`) it is a recursive copy. The child's cwd keeps its relative
    position. Copy-on-write backend names (`apfs`, `btrfs`, ...) fall back to this with a note.
  - When the child succeeds, its changes (everything it edited, added, deleted or committed,
    ignored files excepted) are taken as one patch against the isolation's own starting snapshot,
    saved under `<root>/patches/`, and with `task.isolation.apply` (default on) applied to the
    parent working tree (`merge: patch`, default) or committed on `mhc/task/<id>` and
    cherry-picked onto the parent's HEAD (`merge: branch`; a failed cherry-pick keeps the branch).
    A patch that does not apply is kept and reported; one already present is a no-op. With
    `apply: false` nothing touches the parent. Outside git, changed files are written back only
    if the parent's copies are unchanged since the start (all or nothing; otherwise the copy is
    kept).
  - A failed or cancelled child's changes are discarded. If capturing fails, the isolation is
    kept as `<root>/t<hash>.retained-*` and named in the result. Otherwise the isolation (and its
    worktree registration) is removed. Each isolation carries an owner marker (pid + start token);
    isolations of processes that are gone are swept when `mhc` starts.
  - The outcome is appended to the task's `<task-result>` as an `Isolation (...)` note. A resumed
    isolated task starts from a fresh isolation of the parent's current state.
  - Not included: copy-on-write backends, detaching the worktree's git dir, nested repositories
    and submodules, AI commit messages, a `worktree` command.
- `<builtin:git>` (`maho-git`, ported from the MIT oh-my-pi @ f59bbac agentic-commit tools):
  read-only `git_overview` (changed files, diff stat, numstat; lock files listed apart),
  `git_file_diff` (1-10 files, most important first, long diffs shortened), `git_hunk` (selected
  hunks of one file) and `recent_commits` (subjects with conventional-commit style statistics).
  Staged changes by default, `staged: false` for the working tree. The default (`omo`) build links
  `maho-isolation` and `maho-git` but mounts neither.
- Team runs and task panes (`maho-team`; ports of MIT oh-my-pi @ f59bbac terminal launch and
  multiplexer detection, pi-interactive-subagents @ c100577 and pi-agent-teams @ 235fac8; the team
  layer on `maho-task` from `docs/cleanroom/specs/team.md`), registered with `<builtin:task>`:
  - `team_create` starts a named team (`^[a-z0-9-]+$`) of members, each a background `maho-task`
    task `<team>-<member>` with its own assignment and the shared `context` (or from a configured
    `teams.<name>` template). At most `task.team.max_parallel_members` (default 4) run at once,
    the rest start as slots free up; `task.team.max_members` (default 8) caps the size and
    `task.team.max_wall_clock_minutes` (default 120) stops a team that runs too long.
  - The lead gets a `team-update` message as each member run settles (status, a result preview,
    `done/total` progress) and once when the whole team has finished, unless it is collecting
    with `team_wait`. Member tasks are `quiet`: no generic task-result message.
  - `team_message` sends a member (or `*`) a follow-up: an idle member continues at once in its
    own session; a busy one gets the queued messages as one follow-up run when its run ends.
    `team_members` lists state, task ids, models, runs, queued messages, panes and (`tail`) the
    last output lines; `team_wait` returns each member's `<task-result>` with a `done/total`
    summary (bounded by `task.wait.*`); `team_stop` cancels one member or the team. `/team` lists
    the session's teams. Team state is kept in `teams/<team>.json` in the task state directory.
  - Panes: inside tmux (`$TMUX` + `tmux` on PATH) or cmux (`$CMUX_WORKSPACE_ID`/`$CMUX_SURFACE_ID`
    + `cmux`), each team member (every delegated task with `MAHO_TEAM_PANES=all`; none with `off`)
    is followed in a pane split off the lead without taking focus (`MAHO_TEAM_PANE_LAYOUT=window`
    for windows; `MAHO_TEAM_MUX` picks the backend). The pane shows a title and a live `tail -F`
    of the task's log and output; it closes `MAHO_TEAM_PANE_LINGER` seconds (default 3) after the
    task settles, and by itself when the lead process ends. The child itself always runs as a
    normal `mhc -p` process, so without a multiplexer everything works the same, headless.
  - Not included: zellij/WezTerm backends, interactive teammates, a shared task list with
    dependencies, quality-gate hooks. The default (`omo`) build links `maho-team` but mounts it
    nowhere.

- Skills (`maho-skills`; the `skill` tool's path rules ported from the MIT oh-my-pi @ f59bbac
  `skill://` protocol, the rest from `docs/cleanroom/specs/skills.md`), mounted as
  `<builtin:skills>` on top of the senpi skill loader in `maho-core` (discovery of
  `<agent dir>/skills`, `~/.agents/skills`, trusted-project `.omo/skills` and ancestor
  `.agents/skills`, settings paths, packages, `--skill`/`--no-skills`; `SKILL.md` front matter
  validation; the `<available_skills>` prompt listing; `/skill:<name>`):
  - bundled skill root: `$MAHO_SKILLS_ROOT`, else `skills/` beside the executable, contributed
    with the lowest precedence (a user/project skill of the same name wins); also used by the app
    server's `skills/list`. The MIT staging does not ship a skill set yet (Phase 4);
  - `disabled_skills` from `omo.jsonc` (union over layers) removes those skills from the prompt
    listing and from the `skill` tool;
  - the `skill` tool returns a skill's `SKILL.md` body (front matter removed) in a
    `<skill name location>` block with its directory, plus optional `User:` arguments, or with
    `<skill>/<relative path>` a file or directory listing inside the skill (no absolute paths,
    no `..`, no symlink escape, files up to 256 KiB);
  - `/skill` lists the loaded skills (scope, `disabled`/`user-only`), `/skill <name>` shows one.
  - Not included: `skill://` URLs in `read`/`bash`, namespaced collision aliases, a skill
    registry.
- MCP: the client is the senpi port `maho-ext-mcp`, mounted in every build (stdio and streamable
  HTTP, `<agent dir>/mcp.json` and trusted-project `.omo/mcp.json`, tools named
  `mcp_<server>_<tool>`, `/mcp enable|disable`). `<builtin:mcp-builtins>` (`maho-mcp-builtins`,
  from `docs/cleanroom/specs/mcp.md`) declares two remote servers on it, `context7`
  (`https://mcp.context7.com/mcp`, bearer token from `CONTEXT7_API_KEY` when set) and `grep_app`
  (`https://mcp.grep.app`), lazy and without OAuth. They are not declared with
  `MAHO_BUILTIN_MCPS=0`, in offline mode, with `mcp.builtins: false`, or per name in
  `mcp.disabled_builtins` (`omo.jsonc`); an `mcp.json` entry of the same name replaces one.
  No web-search MCP is added (the `websearch`/`webfetch` builtins cover it). The default (`omo`)
  build links `maho-skills` and `maho-mcp-builtins` but mounts neither.
- Session gateway (`maho-gateway`, from `docs/cleanroom/specs/gateway.md`; delivery shape is the
  MIT senpi session-control contract already in `maho-core`, inbox model from the MIT
  pi-agent-teams mailbox), mounted as `<builtin:gateway>`:
  - long-lived sessions (TUI, `--mode rpc`, app server; not `-p`/JSON runs, so not task children)
    write an endpoint record to `<agent dir>/gateway/sessions/<id>.json` (`$MAHO_GATEWAY_DIR`
    moves the store) and poll `inbox/<id>/` (`gateway.poll_ms`, default 500); each delivery is
    handed to the session as a displayed `session_control_delivery` message (steer or follow-up,
    starting a turn when idle) whose text opens with
    `[gateway message from <sender> · hop <n> · id <id>]` and a note that it is not from the user;
    the record is removed when the session ends; records of dead processes are pruned;
  - tools `session_list` (other live sessions) and `session_send` (`to`: id, name or unique id
    prefix; `message`; `mode`: `followUp` | `steer`); relays are capped by `gateway.max_hops`
    (default 3; reset by user input);
  - shell: `mhc gateway list [--json]`, `mhc gateway send <session> <message…> [--steer]
    [--from <user>]`;
  - `MAHO_GATEWAY=0` or `gateway.enabled: false` turns it off.
  - Not included: holding a delivery while the user has an unsent draft, turn-epoch checks for
    steers, a control socket, external chat bridges.
- Agent home (`maho-agent-home`, clean room from `docs/cleanroom/specs/agent-home.md` over the
  senpi agent dir): one layout for `~/.omo` (`omo.jsonc`, `state/`) and the agent dir (`auth.json`,
  `models.json`, `settings.json`, `sessions/`, `skills/`, `extensions/`, `prompts/`, `bin/`).
  `mhc home [--json]` reports it (exit 1 on a problem: wrong kind, or a group/other-readable
  `auth.json`), `mhc home init` creates the missing directories (0700, never files), `mhc home
  repair` also makes `auth.json` private. The first-run setup state lives in
  `~/.omo/state/setup.json`; existing installs with a legacy `onboarding-completed` marker count as
  set up.
- First-run setup (`maho-onboarding`; versioned steps and gates ported from MIT oh-my-pi's setup
  wizard @ f59bbac, flows from `docs/cleanroom/specs/onboarding.md`):
  - `mhc setup` asks on the terminal for a provider (catalog from `models.json` + the builtin
    providers, `✓` = credential present), an API key when none is stored (no echo) and a default
    model; `mhc setup --provider <id> [--model <provider/id>] [--api-key-env VAR | --api-key-stdin]
    --yes [--json]` does the same without prompts; `mhc setup --check` prints the stored and
    current setup versions;
  - applying a choice stores the key in `auth.json` (`{"type":"api_key"}`, mode 0600), writes
    `defaultProvider`/`defaultModel` to `settings.json` (interactive sessions) and, through the
    comment-preserving `maho-config` writer, `model_profiles.setup.models` + `model_profile:
    "setup"` to `~/.omo/omo.jsonc` (print/JSON/RPC sessions), then records the setup state;
  - first-run gate: an interactive start (no `-p`, no messages, stdin and stdout terminals, not
    resuming) with a stale setup version and no usable model runs the prompts before the session;
    with a usable model it only records the state. `MAHO_SKIP_SETUP=1` or `setup.enabled: false`
    turns the gate off. Not included: OAuth browser logins and the theme/glyph/composer scenes.
- Project context files (`maho-context-files`, clean room from `docs/cleanroom/specs/init-deep.md`
  after MIT oh-my-pi's `/init` prompt and senpi's nested `AGENTS.md` loading), mounted as
  `<builtin:context-files>`:
  - `mhc init-deep [--plan [--json]] [--update] [--dry-run] [--model M] [--root DIR]
    [--max-depth N] [--max-dirs N] [--min-files N]`: analyses the repository (git top-level;
    ignore files honoured; `node_modules`, `target`, ... skipped), picks the root plus the
    significant directories (a build manifest, or at least `min_files` source files; directories
    without files of their own are skipped), and writes each `AGENTS.md` deepest first with a child
    `mhc -p --no-session --tools read,grep,find,ls` run whose prompt carries the directory's facts
    and its children's summaries. Existing files are kept unless `--update`;
  - `/init-deep [update]` sends the same plan to the current session's agent, which writes the
    files with its own tools;
  - in TUI/RPC sessions inside a git repository without `AGENTS.md`/`CLAUDE.md` at the
    root and with at least `min_files` source files, a one-time notification suggests
    `/init-deep` (recorded in `~/.omo/state/context-advice.json`). `MAHO_CONTEXT_ADVICE=0` or
    `init_deep.advise: false` turns it off; `init_deep.max_depth|max_dirs|min_files` tune the choice.

## What is compiled out (needs the `omo` feature)

| Area | In the MIT profile |
| --- | --- |
| omo mount (`<builtin:omo>`): its task/subagent tools, `team_*`, `workflow`/DAG, `workpool`, `/task-kill`, `/dag` | not registered (MIT `<builtin:task>` provides `task`, `task_output`, `task_list`, `task_resume`, `task_cancel`, `/tasks`, and from `maho-team` `team_create`, `team_members`, `team_message`, `team_wait`, `team_stop`, `/team`) |
| omo memory tools and commands (`/remember`, `/dream`, `/reflect`, `/search`, `/people`, `/palace`, `/init`, `/doctor`, `/facts`, `/memfs`, ...), the omo recall sidecar, facts child, git-backed Markdown memory | not registered (MIT `<builtin:memory>` provides `retain`, `recall`, `memory_get`, `memory_forget`, first-turn auto-recall and `/memory`) |
| ast-grep MCP; the omo builtin MCPs; `x_search`; omo browser bridge | not registered (MIT `<builtin:browser>` provides the `browser` and `computer` tools; MIT `<builtin:mcp-builtins>` declares `context7` and `grep_app`) |
| omo gateway, onboarding, init-deep advisor, git-master, omo telemetry | not registered (MIT `<builtin:gateway>` provides `session_list`, `session_send` and `mhc gateway list|send`; MIT `maho-onboarding` provides `mhc setup` and the first-run gate; MIT `<builtin:context-files>` provides `/init-deep`, `mhc init-deep` and the session-start suggestion; MIT `<builtin:telemetry>` provides the usage ledger, `mhc usage` and opt-in OTLP export; MIT `<builtin:git>` provides read-only git inspection tools) |
| bundled omo skills (`OMO_SENPI_SKILLS_ROOT`) and the omo skill tools/commands | not registered (MIT `<builtin:skills>` provides the bundled root via `MAHO_SKILLS_ROOT` or `skills/` beside the executable, `disabled_skills`, the `skill` tool and `/skill`) |
| `builtin:task-member` (`--extension builtin:task-member`) | unknown native extension error |
| Claude Code plugin hooks (`maho-ext-hooks` feature `claude-plugins`) | not discovered; `~/.claude/settings.json` and project `.claude/settings*.json` hooks still load |
| `mhc ulw-loop`, `--memory-supervisor`, `--child-bootstrap` | print "not available in this build", exit 1 (`--omo-memory-mcp` now starts the MIT memory MCP server) |

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
   that the MIT LSP, task, team, git, memory and browser (`browser`, `computer`) tools are offered and no omo tool is, that a `task` call runs a child
   `mhc -p` against the same model and returns its completed `<task-result>`, that in a git
   repository an `isolated: true` task runs its child in a git worktree whose written file is
   applied back to the checkout (worktree removed afterwards), that a model-driven `team_create`
   runs two member children and `team_wait` collects both results (headless, and when `tmux` is on
   PATH again attached to a private tmux server, where each member shows in its own pane that
   closes afterwards), that the git tools are offered and
   `git_overview` reports the change, that a fact stored
   with `retain` is auto-recalled into the next run's first turn and returned by `recall`, that
   `mhc memory import-legacy` + `mhc memory search` work on a legacy Markdown store, that the
   memory MCP server answers over stdio (`--memory-mcp`, `--omo-memory-mcp`), that (when
   `MHC_SMOKE_BROWSER` names a Chromium executable) a model-driven `browser` call returns the page's
   snapshot with refs and a `computer` screenshot of the tab is taken, that a user skill and a
   bundled skill (`MAHO_SKILLS_ROOT`) are listed in the system prompt while a `disabled_skills`
   entry is not, that a model-driven `skill` call returns the bundled skill, that a stdio MCP
   server from `<agent dir>/mcp.json` is started and its tool `mcp_smokefake_echo` answers a model
   call, that the builtin remote MCP servers are absent offline, that an RPC session registers with
   the gateway, receives `mhc gateway send` and a `session_send` relay from a `-p` run (both with the
   gateway header) and is unlisted after it exits while `MAHO_GATEWAY=0` hides the tools, that
   `mhc home init`/`--json`/`repair` build and report the agent home and fix a readable
   `auth.json`, that `mhc setup --provider ... --yes` writes the key, the default model and the
   `setup` model profile (resolved by `mhc doctor`) while refusing an unknown provider or a missing
   `--yes`, that (with `tmux`) an interactive start without a usable model runs the setup prompts
   with the key unechoed, that `mhc init-deep` plans and writes the root and package `AGENTS.md`
   files deepest first through read-only child runs (a rerun keeps them) and an RPC session in the
   fixture repository suggests `/init-deep` once, that model calls are written to the usage
   ledger and exported to a loopback OTLP collector (traces, metrics, logs, no content) while
   `DO_NOT_TRACK=1` stops the export and `MAHO_TELEMETRY=0` stops both, that `mhc usage`
   (`--json`, `--status`) reports the ledger, that `mhc doctor` reports the config sources and resolves a pinned
   model profile, and that `mhc ulw-loop` and `--memory-supervisor` report they are unavailable.
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
