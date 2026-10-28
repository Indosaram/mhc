//! Keyed work pools (MIT, clean room; see `PROVENANCE.md`).
//!
//! Behaviour spec: `docs/cleanroom/specs/workpool.md`. A parent session creates a pool bound to
//! one worker agent, pushes keyed JSON inputs, and receives one `workpool-aggregate` message with
//! the keyed results after `close`. Workers are background delegated tasks (`maho-task`) that
//! report each key through the `workpool` tool's `yield` op.
//!
//! - [`model`]: records, items, workers, ids and error codes.
//! - [`store`]: the `workpools/` directory (records, worker yield logs).
//! - [`host`]: what the engine needs from the runtime, and the worker index.
//! - [`engine`]: lifecycle, dispatch, yield, aggregate delivery, restart and pause.
//! - [`tool`]: the `workpool` tool's validation and response shapes.
//! - [`bridge`]: extra worker tools executed in the parent's runtime.
//! - [`extension`]: the `maho-task` addon, the session wiring and the task-backed host.
pub mod bridge;
pub mod engine;
pub mod extension;
pub mod host;
pub mod model;
pub mod store;
pub mod tool;

#[cfg(test)]
mod tests;

pub use engine::{AGGREGATE_MESSAGE_TYPE, CreateRequest, PoolEngine, PoolEvent, assignment_message, choose_idle};
pub use extension::{TaskHost, WorkerExtension, WorkpoolAddon};
pub use host::{StartedWorker, ToolCatalog, WorkerEntry, WorkerHost, WorkerIndex, WorkerStart};
pub use model::{
    AgentSpec, AggregateState, Binding, ItemError, ItemStatus, PoolError, PoolItem, PoolMode,
    PoolRecord, PoolStatus, PoolWorker, WorkerStatus, codes, validate_pool_id,
};
pub use store::{PoolStore, YieldEvent};
pub use tool::{Caller, TOOL_NAME, ToolOutcome};
