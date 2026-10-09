//! Path helpers for the gateway store.
//!
//! Upstream counterpart: `thread/gateway/paths.ts`.

use std::path::{Path, PathBuf};

pub fn gateway_root_directory(agent_dir: &Path) -> PathBuf {
    agent_dir.join("gateway")
}

pub fn gateway_database_path(agent_dir: &Path) -> PathBuf {
    gateway_root_directory(agent_dir).join("gateway.sqlite")
}

pub fn gateway_inbox_directory(agent_dir: &Path, durable_id: &str) -> PathBuf {
    gateway_root_directory(agent_dir).join("inbox").join(durable_id)
}

pub fn gateway_outbox_marker_path(agent_dir: &Path) -> PathBuf {
    gateway_root_directory(agent_dir).join("outbox.marker")
}
