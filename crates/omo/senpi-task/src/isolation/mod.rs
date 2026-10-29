//! Task isolation (`isolation/` in TypeScript): the sandbox lifecycle the task manager drives.
//!
//! The port of `packages/senpi-task/src/isolation/` at latest (`455dee62`). Six source modules map
//! one-for-one: `baseline-store.ts`, `details.ts`, `index.ts`, `prepare.ts`, `runtime.ts`,
//! `salvage.ts`, `settle.ts`. The manager consumes this module through the `IsolationRuntime`
//! port; the production object is built by `create_isolation_runtime` over `maho-isolation-core`.

pub mod baseline_store;
pub mod details;
pub mod prepare;
pub mod runtime;
pub mod salvage;
pub mod settle;

pub use baseline_store::{baseline_path, isolation_artifacts_dir, read_baseline, write_baseline};
pub use details::{IsolationDetails, IsolationStartedDetails, isolation_details, isolation_line};
pub use prepare::{IsolationPreparation, PrepareIsolationInput, prepare_isolation};
pub use runtime::{
    EnsureInput, IsolationRuntime, IsolationRuntimeOptions, OwnerProbe, create_isolation_runtime,
    isolation_backends,
};
pub use salvage::{
    SalvagePorts, needs_crash_salvage, salvage_crashed_isolation, sweep_isolations, sweep_roots_for,
};
pub use settle::{SettleIsolationInput, settle_isolation};

#[cfg(test)]
mod isolation_tests;
