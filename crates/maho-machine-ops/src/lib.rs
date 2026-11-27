//! Engine-free host operation layer (`senpi` `packages/machine-ops` @ `v2026.11.19-5`).
//!
//! Shared by `maho-tools` (the built-in file and shell tools) and remote/device executors so
//! file operations, process execution, limits, and policy checks follow one contract.

pub mod errors;
pub mod files;
pub mod info;
pub mod limits;
pub mod local;
pub mod policy;
pub mod process;
pub mod types;

pub use errors::{MachineOpError, OpErrorCode, op_error_code};
pub use files::bounded_realpath::{
    RESOLUTION_DEADLINE_MS, fold_path_for_case_insensitive_filesystem, is_missing_path_error,
    symlink_loop_error, with_resolution_deadline, with_resolution_deadline_ms,
};
pub use files::glob::{FdResolver, glob_to_regex};
pub use files::mutation_queue::{
    CancelSignal, FileLockedByStuckModuleError, FileMutationLock, FileMutationLockError,
    acquire_file_mutation_lock, lock_file_mutation, with_file_mutation_queue,
    with_file_mutation_queues,
};
pub use files::realpath_without_open::{realpath_without_open, realpath_without_open_strict};
pub use limits::DEFAULT_LIMITS;
pub use local::{LocalMachineOpsOptions, create_local_machine_ops};
pub use policy::filesystem::{
    FilesystemOperation, FilesystemPolicy, FilesystemPolicyChecker, FilesystemPolicyDecision,
    FilesystemPolicyRequest, canonicalize_filesystem_path, compose_filesystem_policies,
};
pub use process::exec::{curated_env, default_shell};
pub use process::holder_link::{HeldExit, HeldTerminal, HolderLink, HolderSpawnRequest};
pub use process::scopes::{RUNTIME_ENV, SCOPE_ENV, SWEEP_LADDER};
pub use process::wait_child::{WaitForChildProcessOptions, wait_for_child_process};
pub use types::*;
