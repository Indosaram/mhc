# CLI reference

`mhc --help` is the authoritative list; this page groups it and adds the subcommands that the
top-level help does not print. Source: `crates/maho-cli/src/cli/help.txt` and the dispatch in
`crates/maho-cli/src/main.rs`.

```text
mhc [options] [--] [@files...] [messages...]
```

With no prompt `mhc` opens the interactive TUI. `@path` attaches a file or image to the first
message; every other positional argument is a message.

## Subcommands

| Command | Purpose |
| --- | --- |
| `mhc install <source> [-l]` | Install an extension package (`npm:`, `git:`, URL or local path) and add it to settings. `-l` writes project settings (`.omo/settings.json`). |
| `mhc remove <source> [-l]` / `mhc uninstall` | Remove a package and its source from settings. |
| `mhc update [source\|self\|mhc]` | Update mhc, extensions or model catalogs (`--self`, `--extensions`, `--models`, `--all`). |
| `mhc list` | List installed packages from settings. |
| `mhc config [-l]` | TUI to enable/disable package resources; Tab switches global/project scope. |
| `mhc auth check --provider <p>` | Check provider readiness (`--json`, `--credentials`, `--no-refresh`). |
| `mhc auth print-api-key --provider <p>` | Print the resolved API key for an external client. |
| `mhc auth print-bearer-token --provider <p>` | Print an OAuth bearer token, refreshing it when expired (`--min-expiry <duration>`). |
| `mhc auth consent <provider> [--rotation] [--revoke] [--yes]` | Record (or with `--revoke` remove) user risk acknowledgement for first-party-impersonating providers (`anthropic` with OAuth subscription tokens, `anthropic-subscription`, `chatgpt-subscription`, `github-copilot`, `kimi-coding`, `devin`, `cursor`) or for automatic multi-account rotation (`--rotation`). Stored as `providerRiskConsent.<provider>` in global settings. In non-interactive environments, `--yes` is required. |
| `mhc doctor` | Read-only diagnostics: config sources (`~/.omo/omo.jsonc` and project `.omo/omo.jsonc`), config problems and the model profile resolution. |
| `mhc gateway list [--json]` / `mhc gateway send <session> <message…> [--steer] [--from <user>]` | List the live sessions that accept gateway messages, or deliver a message into one (session id, name or unique id prefix). See [features.md](features.md). |
| `mhc usage [--json] [--sessions] [--days N] [--status]` | Report the local usage ledger (requests, tokens, cost estimate, latency). |
| `mhc memory <status\|search\|remember\|import-legacy\|mcp>` | Long-term memory from the shell; `mcp` serves the memory MCP server over stdio. |
| `mhc home [init\|repair] [--json]` | Report, create or repair the agent home layout. |
| `mhc setup` | First-run setup: provider, API key and default model (`--provider ... --yes` for non-interactive use, `--check`). |
| `mhc init-deep` | Generate `AGENTS.md` files for the repository root and significant directories. |
| `mhc models discover` | Discover and test available model provider endpoints. |
| `mhc mini [-c]` | Experimental minimal TUI client. |

`install`, `remove`, `update`, `list`, `config`, `auth` and `doctor` accept `--help`.

`app-server` has been removed. Use `mhc --mode rpc` for process integration over stdin/stdout.
This is not a drop-in replacement for the Codex app-server protocol; clients must use the mhc RPC protocol.

Project trust: `--approve` / `-a` trusts project-local files (`.omo/`, project extensions, hooks)
for one run, `--no-approve` / `-na` ignores them. `/trust` in the TUI saves the decision.

## Main options

| Area | Flags |
| --- | --- |
| Model | `--provider <name>`, `--model <pattern>` (`provider/id`, optional `:<thinking>` suffix), `--thinking off\|minimal\|low\|medium\|high\|xhigh\|max`, `--models <patterns>` (Ctrl+P cycle list, globs allowed), `--api-key <key>`, `--list-models [search]` |
| Mode | `--print` / `-p` (one shot), `--mode text\|json\|rpc`, `--tui-mode regular\|fullscreen`, `--offline` |
| Sessions | `--continue` / `-c`, `--resume` / `-r`, `--session <path\|id>`, `--session-id <id>`, `--fork <path\|id>`, `--session-dir <dir>`, `--no-session`, `--name` / `-n <name>`, `--export <file> [out.html]` |
| Tools | `--tools` / `-t <list>` (allowlist), `--exclude-tools` / `-xt <list>`, `--no-tools` / `-nt`, `--no-builtin-tools` / `-nbt` |
| Prompt | `--system-prompt <text>`, `--append-system-prompt <text\|file>` (repeatable), `--no-context-files` / `-nc` (skip `AGENTS.md` / `CLAUDE.md`) |
| Resources | `--extension` / `-e <path>`, `--no-extensions` / `-ne`, `--skill <path>`, `--no-skills` / `-ns`, `--prompt-template <path>`, `--no-prompt-templates` / `-np`, `--theme <path>`, `--use-theme <name>`, `--no-themes` |
| RPC host | `--multi-session`, `--listen <stdio://\|unix://path>`, `--session-runtime in-process\|worker` |
| Misc | `--verbose`, `--list-tips`, `--version` / `-v`, `--help` / `-h` |

Extensions can register more flags; they are listed at the end of `mhc --help`.

## Interactive slash commands

Built in (`crates/maho-core/src/slash_commands.rs`): `/settings`, `/model`, `/thinking`,
`/scoped-models`, `/favorite-models`, `/tree`, `/fork`, `/clone`, `/new`, `/resume`, `/session`,
`/rename` (`/name`), `/export`, `/import`, `/share`, `/copy`, `/compact`, `/login`, `/logout`,
`/trust`, `/reload`, `/hotkeys`, `/changelog`, `/quit` (`/exit`). Extensions, skills
(`/skill:<name>` or `/<name>`) and prompt templates add more; type `/` in the editor to list them.
`/hotkeys` shows the keybindings, which can be overridden in `keybindings.json` in the agent
directory.
