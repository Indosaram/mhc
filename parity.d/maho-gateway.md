# maho-gateway (MIT migration Phase 3, step 4: session gateway)

| Source (MIT) | Commit | Rust |
|---|---|---|
| code-yeongyu/senpi `packages/coding-agent/src/core/extensions/session-control-types.ts` (delivery custom message, `delivery_id` / `source: "session_control"` / `deliverAs`, sender kinds, inbox directory) | 26acd9353 | crates/maho-gateway/src/message.rs (`to_message`, `Receiver::poll_once`); engine side already in crates/maho-core/src/agent_session.rs |
| tmustier/pi-agent-teams `extensions/teams/mailbox.ts` (per-recipient mailbox, `urgent` = steer, temp file + rename, poll-and-consume) | 235fac8 | crates/maho-gateway/src/store.rs (`Store::deliver`, `Store::pending`) |
| clean-room spec docs/cleanroom/specs/gateway.md | a7590318 | store layout, liveness, target resolution, header text, `session_list` / `session_send`, hop limit, `mhc gateway list|send`, `gateway.*` keys |

Differences from the originals:

- One JSON file per delivery instead of one JSON array per recipient, so writers need no lock.
- No control socket and no `admitExternalMessage` engine call: deliveries enter through the
  extension API's `send_message` (steer / follow-up, trigger turn), whose queues the engine already
  owns; there is no draft hold and no turn-epoch check (the MIT extension API exposes neither).
- Only long-lived sessions (TUI, RPC, app server) receive; one-shot `-p`/JSON runs can only send.
