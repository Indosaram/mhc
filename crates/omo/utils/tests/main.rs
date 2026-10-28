//! Single integration-test binary for this crate: each file under tests/ is one module, so
//! the crate links one test executable instead of one per file.

#[path = "ast_grep.rs"]
mod ast_grep;
#[path = "codegraph_env.rs"]
mod codegraph_env;
#[path = "codegraph_provision.rs"]
mod codegraph_provision;
#[path = "codegraph_resolve.rs"]
mod codegraph_resolve;
#[path = "codegraph_workspace.rs"]
mod codegraph_workspace;
#[path = "config_files.rs"]
mod config_files;
#[path = "migration.rs"]
mod migration;
#[path = "path_containment.rs"]
mod path_containment;
#[path = "process_sweep.rs"]
mod process_sweep;
#[path = "processes.rs"]
mod processes;
#[path = "prompt_async_gate.rs"]
mod prompt_async_gate;
#[path = "prompt_async_gate_dedupe.rs"]
mod prompt_async_gate_dedupe;
#[path = "prompt_async_gate_pending.rs"]
mod prompt_async_gate_pending;
#[path = "prompt_async_gate_queue.rs"]
mod prompt_async_gate_queue;
#[path = "prompt_async_gate_queue_turns.rs"]
mod prompt_async_gate_queue_turns;
#[path = "prompt_async_gate_reservations.rs"]
mod prompt_async_gate_reservations;
#[path = "prompt_async_gate_routes.rs"]
mod prompt_async_gate_routes;
#[path = "root_helpers.rs"]
mod root_helpers;
#[path = "spawn_cancellation.rs"]
mod spawn_cancellation;
#[path = "zip_entry_listing.rs"]
mod zip_entry_listing;
