# Configuration

mhc keeps its configuration under `~/.omo`, the same on-disk layout OmO on senpi uses, so an
existing setup there (providers, models, auth, agents) works without copying anything. The
environment-variable prefix is mhc's own (`MAHO_`).

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
| `~/.omo/omo.jsonc` (or `omo.json`) | Harness config: `agents`, `categories`, `task`, `memory`, `model_profile`, `mcp`, `gateway`, `telemetry`, ... |
| `<project>/.omo/settings.json` | Project settings overrides (used only when the project is trusted). |
| `<project>/.omo/omo.jsonc` | Project harness config, merged over the user file; the nearest ancestor directories are searched. |
| `<project>/AGENTS.md`, `CLAUDE.md` | Project instructions (disable with `--no-context-files`). |

`mhc doctor` reports which config files were found and loaded, config problems, and how the active
model profile resolves against the available models.

## Session retention

Sessions are kept forever by default. Set `sessionRetentionDays` in `settings.json` to a whole number
from 1 to 36500 to delete idle `*.jsonl` sessions older than that many days from the default
sessions directory at startup (a custom `--session-dir` is never pruned). Other values (0, negative,
fractional) are rejected with a warning and nothing is pruned. Running sessions are protected by a
`<session>.jsonl.lock` sidecar file and are skipped. Other programs sharing the sessions directory do not
take mhc's `.jsonl.lock` sidecar lock, so an idle session open only in such a program could be pruned
if retention is enabled.

## Settings reference

The following settings can be configured in `~/.omo/agent/settings.json` (or `.omo/settings.json` in trusted project roots):

| Setting | Type | Default | Description |
| --- | --- | --- | --- |
| `permissionPreset` | string | `"full-access"` | Permission preset for tool execution. Available presets: `full-access` (default, as upstream; unrestricted tool execution without confirmation), `guarded` (allows actions within project and mhc-owned directories, asks before accessing external paths or credential files in interactive mode, refused in headless mode), `workspace`, `accept-edits`, `auto`, `read-only`, `ask`. |
| `bashEnvScrub` | boolean | `true` | When true, scrubs secret-looking environment variables (`*_TOKEN`, `*_KEY`, `*_SECRET`, `*_AUTH`, `DATABASE_URL`, etc.) from model-run commands (bash tool and eval kernel). Set to `false` to restore upstream unscrubbed environment inheritance. |
| `bashEnvPassthrough` | string[] | `[]` | List of environment variable names to pass through to model commands even if their names match secret patterns. E.g. `["GITHUB_TOKEN", "MY_API_KEY"]`. |
| `bashSandbox` | string | `"off"` | Opt-in OS-level write-confinement sandbox for the bash tool. Values: `"off"` (default; unconfined execution), `"auto"` (enables sandbox if platform runner is available), `"required"` (fails closed if sandbox runner is not available). Uses `bwrap` on Linux and `sandbox-exec` on macOS. |
| `providerRiskConsent` | object | `{}` | Map of provider IDs (or `<provider>:rotation`) to ISO timestamp strings recording user risk consent for first-party-impersonating providers or multi-account rotation. Recorded via `/login <provider>` interactively or `mhc auth consent <provider>`. |
| `sessionRetentionDays` | integer | unset | Days to retain idle session transcripts before automatic pruning at startup (1 to 36500). Unset means keep forever. |
| `enableInstallTelemetry` | boolean | `false` | Opt-in flag to enable install telemetry. When false (default), no install telemetry is sent. |

## Environment variables

Prefixed variables are looked up as `MAHO_<NAME>`, then `SENPI_<NAME>`, then `PI_<NAME>`, then `OMO_<NAME>`; the first
one that is set wins. The table uses the canonical spelling shown by `mhc --help`.

| Variable | Effect |
| --- | --- |
| `MAHO_CODING_AGENT_DIR` | Agent directory (default `~/.omo/agent`). Also recognized as `OMO_CODING_AGENT_DIR`, `SENPI_CODING_AGENT_DIR`, `PI_CODING_AGENT_DIR`. |
| `MAHO_CODING_AGENT_SESSION_DIR` | Session storage directory (overridden by `--session-dir`). |
| `MAHO_MEMORY_HOME` | Legacy Markdown memory store read by `mhc memory import-legacy` (default `~/.omo/memory`). |
| `PI_OFFLINE` | `1`/`true`/`yes` disables startup network operations (same as `--offline`). |
| `PI_PACKAGE_DIR` | Override the package directory (resources beside the executable). |
| `MAHO_SKILLS_ROOT` | Override the bundled skills root (default `<executable dir>/skills`). |
| `PI_RULES_DISABLED`, `PI_RULES_MAX_RULE_CHARS`, `PI_RULES_MAX_RESULT_CHARS` | Builtin rules switch and size caps. |
| `PI_TELEMETRY` | Override install telemetry when set to `1`/`true`/`yes` (force enable) or `0`/`false`/`no` (force disable), taking precedence over the `enableInstallTelemetry` setting. |
| `DO_NOT_TRACK` | Standard privacy variable: `1`/`true`/`yes` disables all telemetry and usage event dispatch across all subsystems. |
| `MAHO_TELEMETRY`, `MAHO_USAGE_STATS` | `0` turns off all telemetry, or only the local usage ledger (see [features.md](features.md)). |
| `OMO_CODING_AGENT_DIR` | Legacy / compatibility alias for `MAHO_CODING_AGENT_DIR` to override the agent directory. |
| `PI_SHARE_VIEWER_URL` | Base URL for `/share`. |
| `<PROVIDER>_API_KEY` | Provider credentials: `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `GEMINI_API_KEY`, `DEEPSEEK_API_KEY`, `OPENROUTER_API_KEY`, `AWS_*` for Bedrock, `AZURE_OPENAI_*`, ... `mhc --help` lists all of them. |

Credentials for a built-in provider resolve in this order: `--api-key` (runtime override), the
stored credential in `auth.json` (a value may be a `!command` that prints the key), then the
provider's environment variable. Custom providers in `models.json` carry their own `apiKey`.
