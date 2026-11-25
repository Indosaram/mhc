//! Locality-aware thread tool permission rules (`thread/tools/local-permission.ts`).

use std::path::Path;

use serde_json::Value;

use super::super::address_book::{FindDiskSessionsOptions, find_disk_sessions_with_options};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadPermissionRequest {
    pub permission: String,
    pub patterns: Vec<String>,
    pub always: Vec<String>,
    pub auto_approve_ask: bool,
}

fn never_changes_another_device(name: &str) -> bool {
    matches!(
        name,
        "thread_list"
            | "thread_read"
            | "thread_report"
            | "thread_outbox"
            | "thread_outbox_ack"
            | "thread_answer"
    )
}

fn is_addressed(name: &str) -> bool {
    matches!(
        name,
        "thread_send"
            | "thread_handoff"
            | "thread_interrupt"
            | "thread_close"
            | "thread_rename"
            | "thread_set_model"
            | "thread_set_reasoning"
            | "thread_archive"
    )
}

fn held_here(address: Option<&str>, sessions_dir: Option<&Path>) -> bool {
    let Some(address) = address else {
        return false;
    };
    if address.is_empty() {
        return false;
    }
    let Some(dir) = sessions_dir else {
        return false;
    };
    !find_disk_sessions_with_options(
        dir,
        address,
        &FindDiskSessionsOptions {
            all_scope: false,
            workspace_roots: Vec::new(),
            id_only: true,
        },
    )
    .is_empty()
}

fn local_target(name: &str, input: &Value, sessions_dir: Option<&Path>) -> Option<String> {
    if never_changes_another_device(name) {
        return Some("*".to_owned());
    }
    let obj = input.as_object()?;
    if obj.get("alias").is_some_and(|v| !v.is_null()) {
        return None;
    }
    if name == "thread_create" {
        if obj.get("device").is_some_and(|v| !v.is_null()) {
            return None;
        }
        let thread_src = obj.get("thread").and_then(Value::as_str);
        let fork_src = obj.get("fork_from").and_then(Value::as_str);
        let sources: Vec<&str> = [thread_src, fork_src].into_iter().flatten().collect();
        if sources.is_empty() {
            return Some("*".to_owned());
        }
        if sources.iter().all(|s| held_here(Some(s), sessions_dir)) {
            return Some(sources[0].to_owned());
        }
        return None;
    }
    if is_addressed(name) {
        if name == "thread_handoff" && obj.get("match").and_then(Value::as_str) == Some("fuzzy") {
            return None;
        }
        let address = obj.get("thread").and_then(Value::as_str).unwrap_or("self");
        if address == "self" {
            return Some("self".to_owned());
        }
        if held_here(Some(address), sessions_dir) {
            return Some(address.to_owned());
        }
    }
    None
}

pub fn thread_tool_permission(
    name: &str,
    input: &Value,
    sessions_dir: Option<&Path>,
) -> Vec<ThreadPermissionRequest> {
    match local_target(name, input, sessions_dir) {
        None => vec![ThreadPermissionRequest {
            permission: name.to_owned(),
            patterns: vec!["*".to_owned()],
            always: vec!["*".to_owned()],
            auto_approve_ask: false,
        }],
        Some(target) => vec![ThreadPermissionRequest {
            permission: name.to_owned(),
            patterns: vec![target.clone()],
            always: vec![target],
            auto_approve_ask: true,
        }],
    }
}
