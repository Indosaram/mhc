use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileEntry {
    pub path: String,
    pub operations: BTreeSet<String>,
    pub last_timestamp: i64,
}

pub fn collect_files(branch: &[Value]) -> Vec<FileEntry> {
    let mut calls = BTreeMap::new();
    for entry in branch {
        if entry.get("type").and_then(Value::as_str) != Some("message") { continue; }
        let message = &entry["message"];
        if message.get("role").and_then(Value::as_str) != Some("assistant") { continue; }
        if let Some(content) = message.get("content").and_then(Value::as_array) {
            for block in content {
                if block.get("type").and_then(Value::as_str) != Some("toolCall") { continue; }
                let Some(name) = block.get("name").and_then(Value::as_str) else { continue; };
                let (paths, operation) = match name {
                    "read" | "write" | "edit" => {
                        let Some(path) = block["arguments"].get("path").and_then(Value::as_str).filter(|path| !path.is_empty()) else { continue; };
                        (vec![path.to_owned()], name)
                    }
                    "apply_patch" => {
                        let Some(input) = block["arguments"].get("input").and_then(Value::as_str) else { continue; };
                        (maho_ext_gpt_apply_patch::text::extract_patched_paths(input), "edit")
                    }
                    _ => continue,
                };
                if let Some(id) = block.get("id").and_then(Value::as_str) {
                    calls.insert(id.to_owned(), (paths, operation.to_owned()));
                }
            }
        }
    }
    let mut files = Vec::<FileEntry>::new();
    for entry in branch {
        if entry.get("type").and_then(Value::as_str) != Some("message") { continue; }
        let message = &entry["message"];
        if message.get("role").and_then(Value::as_str) != Some("toolResult") { continue; }
        let Some((paths, name)) = message.get("toolCallId").and_then(Value::as_str).and_then(|id| calls.get(id)) else { continue; };
        let timestamp = message.get("timestamp").and_then(Value::as_i64).unwrap_or(0);
        for path in paths {
            if let Some(existing) = files.iter_mut().find(|file| file.path == *path) {
                existing.operations.insert(name.clone());
                existing.last_timestamp = existing.last_timestamp.max(timestamp);
            } else {
                files.push(FileEntry { path: path.clone(), operations: BTreeSet::from([name.clone()]), last_timestamp: timestamp });
            }
        }
    }
    files.sort_by_key(|file| std::cmp::Reverse(file.last_timestamp));
    files
}
