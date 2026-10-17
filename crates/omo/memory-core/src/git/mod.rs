//! Git repository layer for MemFS storage.

pub mod config_lock;
pub mod errors;
pub mod exec;
pub mod lock_scans;
pub mod open_file_holders;
pub mod path_state;
pub mod path_state_files;
pub mod path_state_index;
pub mod pending_rollback;
pub mod porcelain;
pub mod proc_view;
pub mod repo;
pub mod repo_arguments;
pub mod repo_in_use;
pub mod repo_lock_sweep;
pub mod repo_lock_unlock;
pub mod repo_log;
pub mod repo_maintenance;
pub mod repo_status;
pub mod repo_tree;
pub mod repo_types;
pub mod worktree_mutation_queue;
pub mod worktree_restore;

pub use config_lock::*;
pub use errors::*;
pub use exec::*;
pub use lock_scans::*;
pub use open_file_holders::*;
pub use path_state::*;
pub use pending_rollback::*;
pub use porcelain::*;
pub use proc_view::*;
pub use repo::*;
pub use repo_arguments::*;
pub use repo_in_use::*;
pub use repo_lock_sweep::*;
pub use repo_lock_unlock::*;
pub use repo_log::*;
pub use repo_maintenance::*;
pub use repo_status::*;
pub use repo_tree::*;
pub use repo_types::*;
pub use worktree_restore::*;

#[cfg(test)]
#[path = "exec_tests.rs"]
mod exec_tests;

#[cfg(test)]
#[path = "config_lock_tests.rs"]
mod config_lock_tests;

#[cfg(test)]
#[path = "repo_worktree_serialization_tests.rs"]
mod repo_worktree_serialization_tests;

#[cfg(test)]
#[path = "path_state_tests.rs"]
mod path_state_tests;

#[cfg(test)]
#[path = "repo_tests.rs"]
mod repo_tests;


#[cfg(test)]
#[path = "repo_maintenance_tests.rs"]
mod repo_maintenance_tests;

#[cfg(test)]
#[path = "repo_lock_unlock_tests.rs"]
mod repo_lock_unlock_tests;

