# maho-agent-home (MIT migration Phase 3, step 5a: agent home)

| Source (MIT) | Commit | Rust |
|---|---|---|
| code-yeongyu/senpi `packages/coding-agent/src/config.ts` (agent dir; `auth.json`, `models.json`, `settings.json`, `sessions/`, `bin/` inside it; already ported as `maho-core::config`) | 26acd9353 | crates/maho-agent-home/src/lib.rs (`AgentHome::entries`; the agent dir is an input) |
| clean-room spec docs/cleanroom/specs/agent-home.md | this step | layout table, `bootstrap` (0700 dirs, never files), `inspect` (wrong kind, non-private `auth.json`), `repair`, setup state `~/.omo/state/setup.json` with legacy `onboarding-completed` markers, `mhc home [show|init|repair] [--json]` |

Differences from the omo build: not compared (its agent-home component was not opened). The legacy
marker names come from MIT test fixtures only.
