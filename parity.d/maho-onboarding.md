# maho-onboarding (MIT migration Phase 3, step 5b: first-run setup)

| Source (MIT) | Commit | Rust |
|---|---|---|
| can1357/oh-my-pi `packages/tui/src/setup/wizard.ts` (`ALL_SCENES`, `selectSetupScenes`: TTY gate, resume/skip-variable/disabled gates, `force`, per-scene `minVersion`), `setup-version.ts` (`CURRENT_SETUP_VERSION` = max scene version), `scenes/types.ts` (scene ids, `SetupHost`), `packages/coding-agent/src/commands/setup.ts` (`setup` needs a TTY), `src/main.ts` cold-launch gate | f59bbac3d | crates/maho-onboarding/src/steps.rs (`select`, `skip_value_enabled`, `CURRENT_SETUP_VERSION`), src/host.rs (`SetupHost`), crates/maho-cli/src/cli/first_run.rs (`first_run_gate`) |
| code-yeongyu/senpi (ported in maho-core) API-key credential `{"type":"api_key","key"}`, `settings.json` `defaultProvider`/`defaultModel` | 26acd9353 | crates/maho-cli/src/cli/first_run.rs (`CliSetupHost`) |
| clean-room spec docs/cleanroom/specs/onboarding.md | this step | provider + model steps, line prompts (`src/flow.rs`), `mhc setup` non-interactive path (`src/cli.rs`), `setup` model profile in `omo.jsonc` via the maho-config writer (`src/apply.rs`), `setup.enabled` |

Differences from oh-my-pi: line prompts instead of the fullscreen overlay; only the provider (API
key) and model scenes (no OAuth browser login, theme, glyph or composer scenes); the gate also
requires that no model is usable yet; `MAHO_SKIP_SETUP` instead of `OMP_SKIP_SETUP`.
