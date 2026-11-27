//! Store-free live/disk target resolver for extension transactions (`thread/gateway/extension-resolver.ts`).

use std::path::PathBuf;
use std::sync::Arc;

use super::address_book::{
    FindDiskSessionsOptions, assemble_address_book, find_disk_sessions_with_options,
    to_gateway_address_entries,
};
use super::engine::{GatewayResolution, GatewayResolve, GatewayTarget, resolve_from_entries};
use super::errors::thread_tool_failure;

pub fn create_extension_resolver(agent_dir: &str) -> GatewayResolve {
    let sessions_dir = PathBuf::from(agent_dir).join("sessions");
    Arc::new(move |address: &str, all_scope: bool| {
        let address = address.to_owned();
        let sessions_dir = sessions_dir.clone();
        Box::pin(async move {
            let disk = find_disk_sessions_with_options(
                &sessions_dir,
                &address,
                &FindDiskSessionsOptions {
                    all_scope,
                    workspace_roots: Vec::new(),
                    id_only: false,
                },
            );
            let entries = to_gateway_address_entries(&assemble_address_book(&[], &disk, ""));
            if let Some(found) = entries
                .iter()
                .find(|e| e.thread_id == address || e.name == address)
            {
                return GatewayResolution::Ok(GatewayTarget {
                    durable_id: found.thread_id.clone(),
                    endpoint: found.endpoint.clone(),
                    liveness: found.liveness,
                });
            }
            let _ = resolve_from_entries;
            GatewayResolution::Error(thread_tool_failure(
                "not_found",
                format!("No thread matches {address}."),
                "Call thread_list (tool.thread_list() in an eval cell) and retry.",
                None,
            ))
        })
    })
}
