# Provenance

Method: C (clean room).

Behavioral specification: `docs/cleanroom/specs/workpool.md`.
Specification commit: `8f3d2e29`.

The crate was written in two implementer sessions, both working only in the clean-room checkout
(`cleanroom/impl`). The second session rewrote the first session's single-file store core into
the modules below and added dispatch, yield, aggregate delivery, restart, pause, extra worker
tools, tool validation, extension wiring and the acceptance tests.

Allowed inputs used:

- `docs/cleanroom/specs/workpool.md` (the behavioural source) and `docs/cleanroom/specs/README.md`
  (roles and rules);
- `docs/cleanroom/specs/task.md`, `model-selection.md` (referenced by the spec, for context);
- `AGENTS.md`, the root `Cargo.toml` and `clippy.toml` (workspace conventions and lints);
- MIT code already in this checkout, for host APIs only: `crates/maho-task` (task scope, manager,
  records, settings, addon interface), `crates/maho-workflow` (addon and executor patterns over
  `maho-task`), `crates/maho-gateway` (extension registration and session events),
  `crates/maho-ext-api` (extension, tool, message and event types), `crates/maho-agent` /
  `crates/maho-ai` (tool result types), `crates/maho-core/src/agent_session.rs` (the
  `session_hold_changed` event emission), and `crates/maho-cli/src/cli/default_extensions.rs` /
  `delegation.rs` (registration points; not edited).

No forbidden input was read: nothing under `crates/omo/**`, `assets/omo-latest/**`,
`assets/omo.schema.json` or `.omo/**`, no oh-my-openagent / OmO / lazycodex source or URL, no git
history of removed paths, and no other machine's copy of this repository.
