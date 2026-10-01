//! Port of senpi packages/coding-agent/src/core/credential-pool/.
//!
//! deviation: maho-ai's auth/pool and credential types are still the todo-13/17 stubs, so the
//! slot and credential shapes this module needs are declared in slots.rs with the same field names
//! as senpi's Credential / CredentialSlot / PooledCredential; todo 17 re-points them at maho-ai.

pub mod classify;
pub mod env_slots;
pub mod failover;
pub mod failover_consts;
pub mod rotation_events;
pub mod rotation_stream;
pub mod slots;
pub mod state_store;
