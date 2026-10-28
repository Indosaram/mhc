//! Rust port of the `@oh-my-opencode/senpi-task` package (SUL-1.0, internal use only).

#[cfg(test)]
mod adversarial;
pub mod agents;
pub mod category;
pub mod completion;
pub mod dag;
mod delegate_adapter;
pub mod eval_handles;
pub mod host;
pub mod isolation;
pub mod kernel_tools;
pub mod lifecycle;
pub mod manager;
pub mod model_chain;
pub mod model_chains;
pub mod model_exclusion;
pub mod pause;
pub mod progress;
pub mod renderer_text;
pub mod run_stats;
pub mod runners;
pub mod senpi;
pub mod shared;
pub mod state;
pub mod status_line;
pub mod steering;
pub mod store;
pub mod task_summary;
pub mod team;
#[cfg(test)]
mod test_support;
pub mod tools;
pub mod workpool;

pub use delegate_adapter::DelegateFallbackEntry;
pub use isolation::OwnerProbe;
pub use isolation::{IsolationRuntime, create_isolation_runtime, isolation_backends};

pub use agents::resolve_task_agents;
pub use completion::{
    ParentModelFallback, ParentModelFallbackPhase, ParentModelFallbackReason,
    describe_parent_model_fallback,
};
pub use manager::start_admission::{
    AdmissionHeld, HELD_START_KIND, HeldStart, StartAdmission, StartAdmissionError, StartAdmissions,
    StartOutcome, admission_held_result, enter_start_admission,
};
pub use pause::admission_barrier::{
    AdmissionBarrier, AdmissionBarrierError, AdmissionCheck, AdmissionCommit, AdmissionEntry,
    AdmissionParticipant, AdmissionState, AdmissionTicket, ResumeCredential,
};
pub use pause::ports::{RunPauseOwner, RunRef, RunRefKind};
