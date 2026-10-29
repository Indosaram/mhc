//! Single integration-test binary for this crate: each file under tests/ is one module, so
//! the crate links one test executable instead of one per file.

#[path = "client_diagnostics_concurrency.rs"]
pub mod client_diagnostics_concurrency;
#[path = "client_diagnostics_freshness.rs"]
pub mod client_diagnostics_freshness;
#[path = "formatting_latest.rs"]
pub mod formatting_latest;
#[path = "lifecycle_latest.rs"]
pub mod lifecycle_latest;
#[path = "post_edit_not_installed.rs"]
pub mod post_edit_not_installed;
#[path = "server_installation_latest.rs"]
pub mod server_installation_latest;
#[path = "server_resolution_local_binary.rs"]
pub mod server_resolution_local_binary;
#[path = "workspace_apply_edit.rs"]
pub mod workspace_apply_edit;
#[path = "workspace_apply_edit_lease.rs"]
pub mod workspace_apply_edit_lease;
#[path = "workspace_apply_edit_sync.rs"]
pub mod workspace_apply_edit_sync;

// Helpers several test modules share, declared once so each file compiles once.
#[path = "support/mod.rs"]
pub mod support;
