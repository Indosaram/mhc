//! Port of senpi packages/coding-agent/src/core/session-export.ts.

use std::path::Path;

use serde_json::{Value, json};

use crate::paths::{PathInputOptions, resolve_path};
use crate::session_manager::CURRENT_SESSION_VERSION;

pub trait ExportSessionManager {
    fn get_session_id(&self) -> &str;
    fn get_cwd(&self) -> &str;
    fn get_branch(&self) -> Vec<Value>;
}

fn export_file_name() -> String {
    let stamp = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true).replace([':', '.'], "-");
    format!("session-{stamp}.jsonl")
}

/// Writes the current session branch and optional trailing export-only entries as JSONL.
pub type CreateTrailingEntries<'a> = &'a dyn Fn(Option<&str>, &str) -> Vec<Value>;

pub fn export_session_to_jsonl(
    session_manager: &dyn ExportSessionManager,
    output_path: Option<&str>,
    create_trailing_entries: Option<CreateTrailingEntries<'_>>,
) -> String {
    let cwd = std::env::current_dir().map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| ".".to_owned());
    let file_path = resolve_path(output_path.unwrap_or(&export_file_name()), &cwd, &PathInputOptions::default());
    if let Some(dir) = Path::new(&file_path).parent()
        && !dir.as_os_str().is_empty()
    {
        let _ = std::fs::create_dir_all(dir);
    }

    let timestamp = crate::session_manager::now_iso_millis();
    let header = json!({
        "type": "session",
        "version": CURRENT_SESSION_VERSION,
        "id": session_manager.get_session_id(),
        "timestamp": timestamp,
        "cwd": session_manager.get_cwd(),
    });
    let mut lines = vec![serde_json::to_string(&header).unwrap_or_default()];

    let mut parent_id: Option<String> = None;
    for entry in session_manager.get_branch() {
        let mut with_parent = entry;
        if let Some(object) = with_parent.as_object_mut() {
            object.insert("parentId".into(), parent_id.clone().map(Value::String).unwrap_or(Value::Null));
        }
        parent_id = with_parent.get("id").and_then(Value::as_str).map(str::to_owned);
        lines.push(serde_json::to_string(&with_parent).unwrap_or_default());
    }
    if let Some(create) = create_trailing_entries {
        for entry in create(parent_id.as_deref(), &timestamp) {
            lines.push(serde_json::to_string(&entry).unwrap_or_default());
        }
    }

    let _ = std::fs::write(&file_path, format!("{}\n", lines.join("\n")));
    file_path
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Manager {
        branch: Vec<Value>,
    }

    impl ExportSessionManager for Manager {
        fn get_session_id(&self) -> &str {
            "session-1"
        }
        fn get_cwd(&self) -> &str {
            "/work"
        }
        fn get_branch(&self) -> Vec<Value> {
            self.branch.clone()
        }
    }

    #[test]
    fn exports_a_header_and_the_branch_with_parent_links() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let out = tmp.path().join("out.jsonl").to_string_lossy().into_owned();
        let manager = Manager {
            branch: vec![
                json!({ "type": "message", "id": "a" }),
                json!({ "type": "message", "id": "b" }),
            ],
        };
        let path = export_session_to_jsonl(&manager, Some(&out), None);
        let content = std::fs::read_to_string(&path).expect("read");
        let lines: Vec<Value> = content.lines().map(|line| serde_json::from_str(line).expect("line")).collect();
        assert_eq!(lines[0]["type"], json!("session"));
        assert_eq!(lines[0]["version"], json!(CURRENT_SESSION_VERSION));
        assert_eq!(lines[1]["parentId"], Value::Null);
        assert_eq!(lines[2]["parentId"], json!("a"));
    }

    #[test]
    fn trailing_entries_are_appended_after_the_branch() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let out = tmp.path().join("out.jsonl").to_string_lossy().into_owned();
        let manager = Manager { branch: vec![json!({ "type": "message", "id": "a" })] };
        let create = |parent: Option<&str>, _timestamp: &str| vec![json!({ "type": "custom", "parentId": parent })];
        let path = export_session_to_jsonl(&manager, Some(&out), Some(&create));
        let content = std::fs::read_to_string(&path).expect("read");
        let lines: Vec<Value> = content.lines().map(|line| serde_json::from_str(line).expect("line")).collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[2]["type"], json!("custom"));
        assert_eq!(lines[2]["parentId"], json!("a"));
    }
}
