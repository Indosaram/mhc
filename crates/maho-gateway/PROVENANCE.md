# Provenance — maho-gateway

- License: MIT.
- Method: **C** (clean room) on MIT inputs. Built from `docs/cleanroom/specs/gateway.md`
  (spec commit a7590318), which was written from MIT sources only.
- Origins (license verified as MIT before reading):
  - `code-yeongyu/senpi` @ `26acd935303869fd624f203dd8beb41f66ca4e5d` (LICENSE: MIT):
    `packages/coding-agent/src/core/extensions/session-control-types.ts` — the engine contract
    of an admitted delivery (`session_control_delivery` custom message; details `delivery_id`,
    `source: "session_control"`, `deliverAs`; sender kinds agent / command line; inbox directory
    waking the drain). Its engine half is already ported in `maho-core`; this crate produces
    messages in that shape (`src/message.rs`).
  - `tmustier/pi-agent-teams` @ `235fac8622faed4b0f048ca285237189f590bc5a` (LICENSE: MIT):
    `extensions/teams/mailbox.ts` — file mailbox per recipient, `urgent` = steer, temp file +
    rename writes, poll-and-consume (`src/store.rs`; one file per delivery instead of one JSON
    array per recipient, so no lock file is needed).
- No SUL source (`crates/omo/**`, in particular `maho-omo-gateway`; the omo-gated `maho-cli`
  modules; `assets/omo*`; `.omo/`; OmO/COMP sources) was opened. Store layout, header wording,
  tool and command names are new.
- Tests were written from the spec; no upstream or OmO fixtures were reused.
