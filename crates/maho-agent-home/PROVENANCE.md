# Provenance — maho-agent-home

- License: MIT.
- Method: **C** (clean room) on MIT inputs. Built from `docs/cleanroom/specs/agent-home.md`,
  which was written from MIT sources only.
- Origins (license verified as MIT before reading):
  - `code-yeongyu/senpi` @ `26acd935303869fd624f203dd8beb41f66ca4e5d` (LICENSE: MIT):
    `packages/coding-agent/src/config.ts` — agent dir and the files in it (already ported as
    `maho-core::config`; this crate takes the resolved agent dir as input).
  - `maho-config` (MIT clean room): `<home>/.omo` and `omo.jsonc`.
  - Only the name and location of the legacy `onboarding-completed` marker were taken from MIT
    test fixtures in `crates/maho-cli/tests`.
- No SUL source (`crates/omo/**`, in particular `agent-home`; the omo-gated `maho-cli` modules;
  `assets/omo*`; `.omo/`; OmO/COMP sources) was opened. Entry names, the setup state file and the
  CLI are new.
- Tests were written from the spec; no upstream or OmO fixtures were reused.
