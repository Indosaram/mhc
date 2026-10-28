# Agent home (Phase 3 step 5a)

Status: behaviour spec for the MIT profile. Written from MIT sources and MIT call sites only:

- senpi @ 26acd9353 (MIT) `packages/coding-agent/src/config.ts`, already ported as
  `maho-core::config` (`resolve_agent_dir`, `get_sessions_dir`, `get_auth_path`,
  `get_settings_path`, `get_models_path`, `get_trust_path`, `get_bin_dir`): where the agent dir is
  and which files live in it.
- `maho-config` (MIT clean room, `docs/cleanroom/specs/config.md`): `<home>/.omo` is the user
  configuration directory and `omo.jsonc` its file.
- MIT test fixtures in `crates/maho-cli/tests` (`brand_startup.rs`, `provider_consent.rs`,
  `telemetry_default.rs`, `native_update_support`) that pre-create an empty JSON file named
  `onboarding-completed` in `<home>/.omo` and in `<agent dir>/omo-senpi/omo-native` so a run is
  never held by first-run setup. Only the file name and its location are taken from them.
- Directories already owned by MIT crates: `<agent dir>/gateway` (maho-gateway),
  `<agent dir>/skills`, `<agent dir>/extensions`, `<agent dir>/lsp-client.json` (maho-lsp).

No SUL path, `crates/omo/**`, `assets/omo*`, `.omo/` of the repository or omo-gated maho-cli module
was read. The layout of the omo build's agent home is not reproduced beyond the paths above.

## 1. What it is

The per-user home of the agent: one root directory holding the user configuration file and the
agent dir. It answers "where does X live" for MIT crates, creates the directories a fresh install
needs, and records whether first-run setup has finished.

## 2. Paths

Inputs: the home directory (`maho-config` rules: `HOME`, else `USERPROFILE`, else cwd) and the
agent dir as already resolved by `maho-core` (`$MAHO_CODING_AGENT_DIR` override, else
`<home>/.omo/agent`). The crate does not resolve the agent dir itself.

| Name | Path | Kind |
| --- | --- | --- |
| root | `<home>/.omo` | dir |
| config | `<root>/omo.jsonc` (an existing `omo.json` wins when `omo.jsonc` is absent) | file |
| agent | agent dir | dir |
| auth | `<agent>/auth.json` | file, secret |
| models | `<agent>/models.json` | file |
| settings | `<agent>/settings.json` | file |
| sessions | `<agent>/sessions` | dir |
| skills | `<agent>/skills` | dir |
| extensions | `<agent>/extensions` | dir |
| prompts | `<agent>/prompts` | dir |
| bin | `<agent>/bin` | dir |
| state | `<root>/state` | dir (per-feature state of MIT crates) |
| setup | `<root>/state/setup.json` | file (§4) |

## 3. Bootstrap

`bootstrap` creates every *dir* of §2 that is missing (with parents). On Unix, directories it
creates get mode 0700; existing directories are left alone. It never creates or rewrites a file.
It reports, per dir entry, `created` or `present`. It is idempotent.

`inspect` reports, per entry of §2: name, path, kind, whether it exists, and a problem:
- a dir path that is a file, or a file path that is a dir → `wrong kind`;
- on Unix, `auth` readable or writable by group/other → `secret file is not private (mode NNN)`.

`repair` (only on explicit request) bootstraps, then sets mode 0600 on a non-private `auth` file.

## 4. Setup state

`<root>/state/setup.json`: JSON object `{ "version": <int>, "completed_at": <RFC 3339 UTC>,
"provider": <string|null>, "model": <string|null> }`, written atomically (temp file + rename).

`setup_version()`:
1. `version` from `setup.json` when it parses;
2. else `1` when a legacy marker exists — `<root>/onboarding-completed` or
   `<agent>/omo-senpi/omo-native/onboarding-completed` (contents ignored) — so existing installs are
   not asked again;
3. else `0`.

`mark_setup_complete(version, provider, model)` writes `setup.json` (creating `state/`).

## 5. CLI (`mhc home`)

- `mhc home` / `mhc home show [--json]`: the §2 table with existence and problems; exit 1 when a
  problem exists, else 0. JSON: `{ "root", "entries": [{ "name", "path", "kind", "exists",
  "problem" }], "setup_version" }`.
- `mhc home init`: bootstrap; one line per created dir, then `agent home ready: <root>`.
- `mhc home repair`: bootstrap + repair.
- `--help` → usage on stdout, exit 0; unknown argument → usage on stderr, exit 2.

## 6. Tests (written from this spec)

Fresh bootstrap creates all dirs (0700) and is idempotent; no file is created; inspect flags a
group-readable auth file and repair fixes it; wrong-kind detection; setup version from
`setup.json`, legacy markers, none; JSON shape; CLI exit codes.
