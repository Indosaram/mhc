//! Single integration-test binary for this crate: each file under tests/ is one module, so
//! the crate links one test executable instead of one per file.

#[path = "layout_tmux.rs"]
mod layout_tmux;
#[path = "mailbox.rs"]
mod mailbox;
#[path = "registry.rs"]
mod registry;
#[path = "state_store.rs"]
mod state_store;
#[path = "tasklist.rs"]
mod tasklist;
#[path = "worktree.rs"]
mod worktree;
