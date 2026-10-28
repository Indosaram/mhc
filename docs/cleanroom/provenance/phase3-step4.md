# Phase 3 step 4 — session gateway

Commits: a7590318 (spec), 7b27a55b + d970b0a6 (crate `maho-gateway`), ed9caa73 + de7a0915
(maho-cli wiring, lock), d816cc59 + a53ac1ca (smoke), 0ef42227 (docs, parity), this note.

## What "gateway" covers
Plan §2 lists `maho-omo-gateway` as C (behaviour spec). Without opening it, the MIT call sites
show what the feature is: the senpi session-control contract already ported into `maho-core`
(`session_control_delivery` custom messages carrying `delivery_id`/`source`/`deliverAs`, wake
events) whose delivery authority lived in the omo store, the `gateway` config section in
`maho-config`, and a recorded MIT session fixture with a relayed command-line message. So: relaying
messages into a running session from another session or the shell. Messaging between agent
sessions is general (Claude Code agent teams, pi-agent-teams mailbox, OpenClaw/Hermes chat
gateways), so it is ported, not removed. It is not the AI-provider gateways (Vercel/Cloudflare
AI Gateway, OpenGateway) nor the Radius relay, which are MIT senpi code already in tree.

## Inputs
- `code-yeongyu/senpi` @ 26acd935303869fd624f203dd8beb41f66ca4e5d (MIT, already pinned):
  `packages/coding-agent/src/core/extensions/session-control-types.ts`.
- `tmustier/pi-agent-teams` @ 235fac8622faed4b0f048ca285237189f590bc5a (MIT, already pinned):
  `extensions/teams/mailbox.ts`.
- Spec: `docs/cleanroom/specs/gateway.md`.

No SUL path, `crates/omo/**`, `assets/omo*`, `.omo/` or omo-gated maho-cli module was opened.
No upstream was newly cloned (both sources were already in `/workspace/work/upstream` and pinned
in PINS.md).

## Decisions
- New crate `crates/maho-gateway` (MIT, publish = false); mounted as `<builtin:gateway>` by the
  neutral module `maho-cli/src/cli/session_gateway.rs` in the MIT profile only; `mhc gateway`
  dispatched in `main.rs` under `cfg(not(feature = "omo"))`. The default `omo` build links the crate
  (non-optional dependency, like maho-skills) but does not mount or dispatch it.
- Deliveries go through the extension API's `send_message` (steer/follow-up, trigger turn) instead
  of porting senpi's control socket and `admitExternalMessage`; draft hold and turn-epoch checks
  are therefore absent (documented gap).
- Only long-lived modes (TUI, RPC, app server) register and receive; `-p`/JSON runs (including
  task children) can send but are never listed.
- Relay loops are bounded by a hop counter (`gateway.max_hops`, default 3).

## Verification (local only, no CI)
maho-gateway 15 tests, clippy --all-targets -D warnings, rustfmt clean; maho-cli MIT clippy
(--lib --bins -D warnings) clean, MIT lib tests 191 passed; check-mit-profile ok; full
mit-profile-smoke ok (browser + tmux, new gateway block: RPC session registers, `mhc gateway send`
and a `session_send` relay both reach its model with the header, unlisted after exit,
`MAHO_GATEWAY=0` hides the tools); `cargo check -p maho-cli` (default features) in mhcg.

## Workspace notes
- A mid-step workspace rollback removed uncommitted files (tools.rs, cli.rs, extension.rs,
  lib.rs, tests, workspace member); they were rewritten and committed immediately.
