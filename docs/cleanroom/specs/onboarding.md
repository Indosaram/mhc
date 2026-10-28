# First-run setup (Phase 3 step 5b)

Status: behaviour spec for the MIT profile. Written from MIT sources only:

- oh-my-pi @ f59bbac3d (MIT) `packages/tui/src/setup/wizard.ts`, `setup-version.ts`,
  `scenes/types.ts`, `packages/coding-agent/src/modes/setup.ts`, `src/commands/setup.ts`, and the
  cold-launch gate in `src/main.ts`: setup is an ordered list of versioned steps; a step runs when
  its `min_version` is newer than the stored setup version; selection is empty without a TTY, when
  resuming a session, when a skip variable is truthy or setup is disabled, unless forced; the
  stored version advances to the current version only after the steps finish; `setup` without a
  TTY is an error.
- senpi @ 26acd9353 (MIT), ported in `maho-core`: API-key credentials in `<agent dir>/auth.json`
  as `{"type":"api_key","key":…}` (`AuthStorage::set`), provider environment keys
  (`maho-ai::env_api_keys`), `settings.json` `defaultProvider` / `defaultModel`.
- `maho-config` writer (MIT clean room, config.md §11) for `omo.jsonc` edits, and the model
  profile keys `model_profile` / `model_profiles.<name>.models` (config-keys.md) read by
  `maho-model-select`.
- Agent home setup state: `docs/cleanroom/specs/agent-home.md` §4.

No SUL path, `crates/omo/**`, `assets/omo*`, `.omo/` or omo-gated maho-cli module was read. The
omo build's onboarding screens, wording and stored fields are not reproduced.

## 1. Steps

| id | min_version | Asks | Applies |
| --- | --- | --- | --- |
| `provider` | 1 | which provider to use and, unless it already has a credential, an API key | stores the key in `auth.json` |
| `model` | 1 | which model of that provider is the default | writes the default model (§4) |

`CURRENT_SETUP_VERSION` = max `min_version` = 1. A step whose input is already decided (a provider
given on the command line) does not prompt.

## 2. Selection (who sees setup)

`select(stored_version, options)` returns the steps to run:
- no TTY (stdin and stdout) → none, unless `force`;
- unless `force`: resuming a session (`--continue`, `--resume`, `--session`, `--fork`) → none;
  `MAHO_SKIP_SETUP` truthy (not empty / `0` / `false` / `no`) → none; `setup.enabled: false` in
  `omo.jsonc` → none;
- unless `force`, a step with `min_version <= stored_version` is skipped;
- the first-run gate additionally requires that no model is usable yet (no credential for any
  catalog model and no custom provider in `models.json`); an install that can already talk to a
  model only gets its stored version advanced, without prompts.

## 3. Flows

Interactive (`mhc setup`, or the first-run gate before an interactive session): plain line prompts
on stderr, answers from stdin.
1. Provider: numbered list of providers in the catalog (id, display name, `✓` when a credential
   is present in `auth.json` or the environment). Answer: number or id.
2. Key: when the provider has no credential, read a key with echo off; empty answer → skip the
   key (the provider may use an environment key later).
3. Model: numbered list of that provider's models (first 20, then "type an id"); default answer
   is the first listed. Answer: number or id.
4. Summary, then apply (§4) and mark complete.
`Ctrl-D`/EOF at any prompt aborts without writing anything and without marking complete.

Non-interactive (`mhc setup --provider <id> [--api-key-env <VAR> | --api-key-stdin]
[--model <provider/id | id>] [--yes]`): no prompts; works without a TTY. `--yes` is required when
stdin is not a TTY. Unknown provider or model → error, exit 2, nothing written. Without `--model`
the provider's first catalog model is used. `--check` prints the stored and current setup version
and whether setup would run, exit 0.

Other flags: `--force` (run all steps even when current), `--json` (non-interactive result as JSON:
`{ "provider", "model", "stored_key": bool, "version" }`).

## 4. Applying a choice

1. Key (when given): `auth.json` entry `<provider>` = `{"type":"api_key","key":<key>}`; the file
   stays mode 0600.
2. Default model `<provider>/<id>`:
   - `settings.json` (global): `defaultProvider` = provider, `defaultModel` = id — used by
     interactive sessions;
   - `omo.jsonc` (user file, comment-preserving writer): `model_profiles.setup.models` =
     `["<provider>/<id>"]` and `model_profile` = `"setup"` — used where the session-start model
     profile applies (print, JSON, RPC). Other keys are untouched; the writer makes its usual backup.
3. Agent home bootstrap, then `mark_setup_complete(CURRENT_SETUP_VERSION, provider, model)`.

## 5. Keys

`setup` section in `omo.jsonc`: `setup.enabled` boolean, default true. Other keys accepted and
ignored.

## 6. Tests (written from this spec)

Selection gates (TTY, resume, skip variable values, disabled, versions, force); interactive flow
over scripted input (numbers and ids, skipped key, EOF aborts with no writes); non-interactive
validation (unknown provider/model, missing `--yes` without TTY); apply writes auth, settings and
`omo.jsonc` keys and preserves comments; setup state written last.
