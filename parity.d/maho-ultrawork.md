# maho-ultrawork (clean-room ultrawork engine)

| Replaced crate | Status | Rust |
|---|---|---|
| `maho-omo-ultrawork` (parity.d rows 42, 1043, 9700) | REPLACED by crates/maho-ultrawork (MIT clean-room) | crates/maho-ultrawork/src/{classify,engine,extension,state,texts}.rs |
| `maho-omo-mass-ulw` (parity.d rows 42, 9700) | REPLACED by crates/maho-ultrawork (MIT clean-room) | crates/maho-ultrawork/src/texts.rs (`mass_block`), classify.rs (`mass`) |
| `maho-omo-todo-fanout-reminder` (parity.d rows 42, 9442, 9700) | REPLACED by crates/maho-ultrawork (MIT clean-room) | crates/maho-ultrawork/src/engine.rs (`on_tool_result`), texts.rs (`todo_advisory`) |

Behavior comes from `docs/cleanroom/specs/ultrawork.md`; the three SUL crates were deleted and are
no longer measured against upstream sources.
