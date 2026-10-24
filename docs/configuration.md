# Configuration

mhc shares OmO's on-disk configuration: by default it reads and writes the same files the
TypeScript OmO on senpi uses, so an existing OmO setup (providers, models, auth, agents) works
without copying anything. Only the environment-variable prefix stays mhc's own (`MAHO_`).

## Locations

| Path | Contents |
| --- | --- |
| `~/.omo/agent/` | The agent directory. Override with `MAHO_CODING_AGENT_DIR` (also accepted: `SENPI_CODING_AGENT_DIR`, `PI_CODING_AGENT_DIR`, `OMO_CODING_AGENT_DIR`). |
| `~/.omo/agent/settings.json` (or `settings.jsonc`) | Global settings: default provider/model/thinking level, theme, packages, retry, terminal, permissions, ... `/settings` edits it. |
| `~/.omo/agent/models.json` | Custom providers and models (`providers.<id>.baseUrl`, `api`, `apiKey`, `models[]`); providers listed in `disabledProviders` are hidden. `mhc --list-models` shows the merged catalog. |
| `~/.omo/agent/auth.json` | Stored API keys and OAuth credentials (`/login`, `/logout`). |
| `~/.omo/agent/keybindings.json` | Keybinding overrides (`/hotkeys` lists the current bindings). |
| `~/.omo/agent/trust.json` | Saved project-trust decisions (`/trust`). |
| `~/.omo/agent/mcp.json` | MCP servers; also read from `<project>/.omo/mcp.json` and `<project>/.mcp.json`. |
| `~/.omo/agent/sessions/` | Session transcripts, one directory per project. Override with `--session-dir` or `MAHO_CODING_AGENT_SESSION_DIR`. |
| `~/.omo/agent/skills/`, `prompts/`, `themes/`, `extensions/` | User skills, prompt templates, themes and extension markers. |
| `~/.omo/omo.jsonc` (or `omo.json`) | OmO harness config: `agents`, `categories`, `task`, `memory`, `team_mode`, ... The JSON Schema ships as `omo.schema.json` beside the binary. |
| `<project>/.omo/settings.json` | Project settings overrides (used only when the project is trusted). |
| `<project>/.omo/omo.jsonc` | Project OmO config, merged over the user file; the nearest ancestor directories are searched. |
| `<project>/AGENTS.md`, `CLAUDE.md` | Project instructions (disable with `--no-context-files`). |

`mhc doctor` reports which omo config files were loaded, the trust state of the current project and
whether every category resolves to an available model.

If you keep a separate agent directory (for example `MAHO_CODING_AGENT_DIR=~/.maho/agent`),
`mhc import-omo` copies settings, models, keybindings, auth and sessions over from `~/.omo/agent`.

## Session retention

Sessions are kept forever by default. Set `sessionRetentionDays` in `settings.json` to a whole number
from 1 to 36500 to delete idle `*.jsonl` sessions older than that many days from the default
sessions directory at startup (a custom `--session-dir` is never pruned). Other values (0, negative,
fractional) are rejected with a warning and nothing is pruned. Running sessions are protected by a
`<session>.jsonl.lock` sidecar file and are skipped.

## Environment variables

Prefixed variables are looked up as `MAHO_<NAME>`, then `SENPI_<NAME>`, then `PI_<NAME>`, then `OMO_<NAME>`; the first
one that is set wins. The table uses the canonical spelling shown by `mhc --help`.

| Variable | Effect |
| --- | --- |
| `MAHO_CODING_AGENT_DIR` | Agent directory (default `~/.omo/agent`). |
| `MAHO_CODING_AGENT_SESSION_DIR` | Session storage directory (overridden by `--session-dir`). |
| `MAHO_MEMORY_HOME` | Root of the persistent memory store (default `~/.omo/memory`). |
| `PI_OFFLINE` | `1`/`true`/`yes` disables startup network operations (same as `--offline`). |
| `PI_PACKAGE_DIR` | Override the package directory (resources beside the executable). |
| `OMO_SENPI_SKILLS_ROOT` | Override the builtin skills root (default `<executable dir>/skills`). |
| `OMO_SENPI_DISABLED` | Disable the OmO feature layer. |
| `PI_RULES_DISABLED`, `PI_RULES_MAX_RULE_CHARS`, `PI_RULES_MAX_RESULT_CHARS` | Builtin rules switch and size caps. |
| `PI_TELEMETRY` | Force install telemetry on or off. |
| `PI_SHARE_VIEWER_URL` | Base URL for `/share`. |
| `<PROVIDER>_API_KEY` | Provider credentials: `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `GEMINI_API_KEY`, `DEEPSEEK_API_KEY`, `OPENROUTER_API_KEY`, `AWS_*` for Bedrock, `AZURE_OPENAI_*`, ... `mhc --help` lists all of them. |

Credentials for a built-in provider resolve in this order: `--api-key` (runtime override), the
stored credential in `auth.json` (a value may be a `!command` that prints the key), then the
provider's environment variable. Custom providers in `models.json` carry their own `apiKey`.
