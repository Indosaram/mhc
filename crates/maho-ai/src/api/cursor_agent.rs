//! Port of senpi packages/ai/src/api/cursor-agent.ts.
// ported by todo 12

pub mod deterministic_id;
pub mod exec_lifecycle;
pub mod exec_modern;
pub mod r#gen;
pub mod measure;
pub mod pi_args;
// `reasoning_params` and `types` are written but not wired: both reference
// `super::gen::agent_pb` message types that do not exist yet (node 12-cursor-pb
// has not landed generated protobuf bindings), and `reasoning_params` also
// calls `crate::cursor::selection_descriptor` (todo 13, still a placeholder).
// Wiring either in now would break `cargo check -p maho-ai` for every other
// lane sharing this crate. See parity.d/12.md.
// pub mod reasoning_params;
pub mod stream_retry;
// pub mod types;
