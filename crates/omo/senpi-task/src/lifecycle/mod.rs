//! Task lifecycle (`lifecycle/` in TypeScript): residency, destruction, TTL, shutdown, reconcile.

pub mod admission_lease;
pub mod context;
pub mod create;
pub mod destroy;
pub mod errors;
pub mod port;
pub mod reconcile;
pub mod reconcile_crashed_resident;
pub mod reconcile_reclamation;
pub mod reconcile_revival;
pub mod residency;
pub mod settings;
pub mod shutdown;
pub mod store_port;
pub mod ttl;
pub mod types;

pub use context::{LifecycleContext, LifecycleDeps, resolve_context};
pub use create::{TaskLifecycle, create_task_lifecycle};
pub use errors::{AgentLimitReached, LifecycleError, ResidentSummary};
pub use port::{
    DestroyCause, LifecycleReattachPorts, OrphanSignal, ProcessSignaller, ReattachPort,
    ReattachResult, ResidencyRegistry, ResidentHandle, ResidentKind, RespawnPort, RespawnResult,
    get_lifecycle_reattach_ports, register_lifecycle_reattach_ports,
};
pub use settings::{ResidencyCap, TaskSettings};
pub use store_port::LifecycleStore;
pub use types::{
    AdmissionResult, CleanupResult, ReconcileOutcome, ReconcileOutcomeKind, ReconcileResult,
    SuspendFailure, SuspendInput, SuspendSummary,
};

#[cfg(test)]
pub(crate) mod lifecycle_tests;
