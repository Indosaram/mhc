# Intentional safety and privacy divergences from upstream senpi / oh-my-openagent

mhc is a file-for-file Rust port of senpi and oh-my-openagent. The entries below are places where
the port **intentionally** departs from upstream behavior to establish safer defaults, protect
sensitive environment variables, require explicit consent for impersonating providers and account
rotation, isolate execution, or preserve user privacy. Unlike performance divergences, these changes
deliberately alter externally observable behavior.

Every site carries a source comment of the form:

```rust
// SAFETY-DIVERGENCE(<upstream file>): <what upstream does>; <what mhc does instead>.
```

Search with `rg -n "SAFETY-DIVERGENCE" crates`. When porting a later upstream change into a marked
file, re-apply the upstream change on top of the mhc behavior; never revert it.

Prose-only changes (docs, help text) need no marker. Each later todo fills its row's Files column
with the real paths it touched.

## Planned and implemented divergences

| ID | Files | Upstream | mhc behavior | Restore upstream behavior with |
|---|---|---|---|---|
| SD-1 | TBD (todo 8) | Full-access permission preset by default; all tool calls permitted without confirmation | Guarded permission preset by default; tools run within project/mhc directories, ask before accessing external paths or credential files in interactive mode, refused headlessly | `--permission-preset full-access` CLI flag or `permissionPreset: "full-access"` in settings.json |
| SD-2 | TBD (todo 10) | Shell commands inherit full process environment, stripping only 8 internal keys | Model-run commands (bash tool and eval kernel) scrub secret-looking environment variables (`*_TOKEN`, `*_KEY`, `*_SECRET`, `*_AUTH`, `DATABASE_URL`, etc.) | `bashEnvScrub: false` or specify allowed variables in `bashEnvPassthrough: ["VAR"]` in settings.json |
| SD-3 | TBD (todo 9) | Hardcoded `approval_policy: "never"` and `sandbox_mode: "danger-full-access"` in app-server | App-server reports effective approval policy from active permissions ("on-request" or "untrusted") and truthful OS isolation status ("workspace-write" when sandbox active, else "danger-full-access") | `permissionPreset: "full-access"` for approval policy "never"; sandbox mode always reflects OS sandbox status |
| SD-4 | TBD (todo 13) | First-party-impersonating subscription logins (Claude Code, ChatGPT/Codex, Copilot, Kimi CLI, Devin, Cursor) load unconditionally | Impersonating providers are blocked until explicit risk consent is recorded | Run `/login <provider>` interactively or `mhc auth consent <provider>` |
| SD-5 | TBD (todo 14) | Automatic multi-account failover/rotation across token pools enabled by default on rate limits | Multi-account rotation is disabled by default; uses only primary account without recorded rotation consent | `mhc auth consent <provider> --rotation` |
| SD-6 | TBD (todo 15) | Usage telemetry enabled by default (`telemetry.enabled: true`, `enableInstallTelemetry: true`), third-party attribution headers sent, GeoIP enabled | Telemetry and attribution opt-in only (disabled by default); GeoIP permanently disabled (`disable_geoip: true`) | `telemetry.enabled: true` in `~/.omo/omo.jsonc` or `enableInstallTelemetry: true` in settings.json (GeoIP remains off) |
| SD-7 | TBD (todo 16) | People profiling memory enabled by default (`memory.people.enabled: true`), writing third-party observations to `people/<slug>/` | People profiling memory disabled by default (`memory.people.enabled: false`); third-party facts dropped while primary human facts are preserved | `memory.people.enabled: true` in `~/.omo/omo.jsonc` |
| SD-8 | TBD (todo 17) | `mhc import-omo` with no arguments attempts to copy `~/.omo/agent` to itself and fails with "source and destination must be separate directories" | `mhc import-omo` with no arguments detects `~/.omo/agent` is already read directly, prints an informative message, and exits 0 | Pass `--from <dir>` to import from another directory |
| SD-9 | TBD (todo 18) | Ask-user timeout format string includes Korean suffix `" (사용자가 답변을 안하고 timeout 으로 종료됨)"` | English-only timeout message `"The user did not answer within {minutes} minutes."` | N/A (prose formatting fix; placeholder subject to verification in todo 18) |
| SD-10 | TBD (todo 11) | No OS-level bash sandbox write confinement in MIT crates | Opt-in OS write-confinement sandbox for bash using `bwrap` (Linux) or `sandbox-exec` (macOS) via `bashSandbox` setting ("off", "auto", or "required") | Default is `bashSandbox: "off"` (matches upstream unconfined execution) |
| SD-11 | TBD (todo 17) | Session transcript files (`*.jsonl`) retained indefinitely with no automatic cleanup | Opt-in session retention pruning via `sessionRetentionDays`, removing idle unreferenced files older than N days | Default is unset (keeps sessions forever, matching upstream) |
| SD-12 | TBD (todo 17) | `OMO_` environment prefix documented but omitted from `LEGACY_ENV_PREFIXES` | `OMO` added to `LEGACY_ENV_PREFIXES` so `OMO_*` environment variables (e.g., `OMO_CODING_AGENT_DIR`) are recognized | Avoid setting `OMO_*` environment variables or use `MAHO_*` / `SENPI_*` directly |
| SD-13 | TBD (todo 12) | Windows `set_mode` is a no-op, leaving `auth.json` with default inherited ACL permissions | Sets protected owner-only DACL on Windows, granting full control exclusively to current user SID (matching Unix 0600) | Manually adjust permissions with `icacls` if wider access is needed |

## Further reading

- [docs/parity.md](parity.md) — Upstream parity tracking and verification
- [docs/perf-divergences.md](perf-divergences.md) — Intentional performance divergences from upstream
