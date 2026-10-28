# Provenance — maho-onboarding

- License: MIT.
- Method: **P + C**. The step model is ported from MIT oh-my-pi; the flows and the applied
  settings are built from `docs/cleanroom/specs/onboarding.md`, written from MIT sources only.
- Origins (license verified as MIT before reading):
  - `can1357/oh-my-pi` @ `f59bbac3d56e8b485fef466388b1cb5be4b93123` (LICENSE: MIT):
    `packages/tui/src/setup/wizard.ts` (`ALL_SCENES`, `selectSetupScenes`: TTY gate, resume and
    skip-variable gates, `force`, per-scene `minVersion` against the stored version;
    `runSetupWizard` marking completion only after the scenes finish), `setup-version.ts`
    (`CURRENT_SETUP_VERSION` = max scene version), `scenes/types.ts` (scene ids, host interface),
    `packages/coding-agent/src/commands/setup.ts` (`setup` needs a TTY) → `src/steps.rs`,
    `src/host.rs`. The fullscreen overlay, theme/glyph/composer scenes and OAuth browser login are
    not ported (line prompts; API keys only).
  - `code-yeongyu/senpi` @ `26acd935303869fd624f203dd8beb41f66ca4e5d` (LICENSE: MIT), as already
    ported in `maho-core`: API-key credential shape, `settings.json` default model keys (used by
    the maho-cli host).
  - `maho-config` writer and model profile keys (MIT clean room) → `src/apply.rs`.
- No SUL source (`crates/omo/**`, in particular `onboarding`; the omo-gated `maho-cli` modules;
  `assets/omo*`; `.omo/`; OmO/COMP sources) was opened. Prompts and names are new.
- Tests were written from the spec; no upstream or OmO fixtures were reused.
