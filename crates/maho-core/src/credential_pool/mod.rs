//! Port of senpi packages/coding-agent/src/core/credential-pool/.
//!
//! `ModelRuntime::stream`/`stream_simple` drive `rotation_stream::stream_with_credential_rotation`
//! for providers holding more than one credential slot; slots.rs keeps the stored pooled shape.

pub mod classify;
pub mod env_slots;
pub mod failover;
pub mod failover_consts;
pub mod reset_time;
pub mod rotation_events;
pub mod rotation_stream;
pub mod slots;
pub mod state_file;
pub mod state_store;
