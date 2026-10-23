//! Port of handoff-hint/brief.ts.
use maho_ext_api::SessionEntry;
use maho_ext_goal::types::{Goal, GoalStatus};
use maho_ext_todotools::todo_storage::get_latest_state_from_branch_entries;
use maho_ext_todotools::todo_types::TodoStatus;
use serde_json::Value;

pub struct HandoffBriefInput<'a> {
    pub entries: &'a [SessionEntry],
    pub goal: Option<&'a Goal>,
    pub cwd: &'a str,
    pub model_label: Option<&'a str>,
    pub previous_session_file: Option<&'a str>,
    pub context_tokens: u64,
}

pub const MAX_TEXT_CHARS: usize = 1500;
pub const MAX_FILES: usize = 20;
pub const MAX_TODOS: usize = 20;
pub const MAX_TODO_CHARS: usize = 200;
const PATH_TOOLS: [&str; 5] = ["read", "edit", "write", "multiedit", "ls"];
const PATCH_PREFIXES: [&str; 3] = ["*** Update File: ", "*** Add File: ", "*** Delete File: "];

fn trim(text: &str, max: usize) -> String {
    let flat = text.trim();
    if flat.chars().count() > max {
        let head: String = flat.chars().take(max).collect();
        format!("{head}...")
    } else {
        flat.to_owned()
    }
}

fn text_of(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn paths_of_call(name: &str, args: Option<&Value>) -> Vec<String> {
    let Some(record) = args.and_then(Value::as_object) else {
        return Vec::new();
    };
    if PATH_TOOLS.contains(&name) {
        let path = record
            .get("path")
            .filter(|value| !value.is_null())
            .or_else(|| record.get("file_path"));
        return path
            .and_then(Value::as_str)
            .map(|path| vec![path.to_owned()])
            .unwrap_or_default();
    }
    let patch = record
        .get("patch")
        .filter(|value| !value.is_null())
        .or_else(|| record.get("input"));
    if name == "apply_patch"
        && let Some(patch) = patch.and_then(Value::as_str)
    {
        return patch
            .lines()
            .filter_map(|line| {
                PATCH_PREFIXES
                    .iter()
                    .find_map(|prefix| line.strip_prefix(prefix))
            })
            .filter(|path| !path.is_empty())
            .map(|path| path.trim().to_owned())
            .collect();
    }
    Vec::new()
}

fn message_of<'a>(entry: &'a SessionEntry, role: &str) -> Option<&'a Value> {
    if entry.data.get("type").and_then(Value::as_str) != Some("message") {
        return None;
    }
    let message = entry.data.get("message")?;
    (message.get("role").and_then(Value::as_str) == Some(role)).then_some(message)
}

fn touched_files(entries: &[SessionEntry]) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    for entry in entries.iter().rev() {
        if seen.len() >= MAX_FILES {
            break;
        }
        let Some(message) = message_of(entry, "assistant") else {
            continue;
        };
        let Some(parts) = message.get("content").and_then(Value::as_array) else {
            continue;
        };
        for part in parts.iter().rev() {
            if part.get("type").and_then(Value::as_str) != Some("toolCall") {
                continue;
            }
            let name = part.get("name").and_then(Value::as_str).unwrap_or_default();
            for path in paths_of_call(name, part.get("arguments")) {
                if seen.len() >= MAX_FILES {
                    break;
                }
                if !seen.contains(&path) {
                    seen.push(path);
                }
            }
        }
    }
    seen
}

fn last_text(entries: &[SessionEntry], role: &str) -> String {
    for entry in entries.iter().rev() {
        let Some(message) = message_of(entry, role) else {
            continue;
        };
        let text = text_of(message.get("content"));
        if !text.trim().is_empty() {
            return trim(&text, MAX_TEXT_CHARS);
        }
    }
    String::new()
}

pub fn build_handoff_brief(input: &HandoffBriefInput<'_>) -> String {
    let previous = input
        .previous_session_file
        .filter(|file| !file.is_empty())
        .map(|file| format!(" ({file})"))
        .unwrap_or_default();
    let mut lines: Vec<String> = vec![
        format!("Handoff from an earlier session{previous}."),
        format!(
            "That session had grown to about {}k tokens, so only this brief carries over. Read the old session file if you need more detail.",
            (input.context_tokens as f64 / 1000.0).round()
        ),
    ];
    if let Some(goal) = input.goal.filter(|goal| goal.status == GoalStatus::Active) {
        lines.push(String::new());
        lines.push(format!("Goal: {}", trim(&goal.objective, MAX_TEXT_CHARS)));
    }
    let raw: Vec<Value> = input
        .entries
        .iter()
        .map(|entry| entry.data.clone())
        .collect();
    let open: Vec<String> = get_latest_state_from_branch_entries(&raw)
        .phases
        .iter()
        .flat_map(|phase| {
            phase.tasks.iter().filter_map(move |task| {
                let label = match task.status {
                    TodoStatus::InProgress => "in progress",
                    TodoStatus::Pending => "open",
                    _ => return None,
                };
                Some(format!(
                    "- [{label}] {}: {}",
                    phase.name,
                    trim(&task.content, MAX_TODO_CHARS)
                ))
            })
        })
        .collect();
    if !open.is_empty() {
        lines.push(String::new());
        lines.push("Open todos:".into());
        lines.extend(open.iter().take(MAX_TODOS).cloned());
        if open.len() > MAX_TODOS {
            lines.push(format!("- and {} more", open.len() - MAX_TODOS));
        }
    }
    let ask = last_text(input.entries, "user");
    if !ask.is_empty() {
        lines.extend([String::new(), "Last request:".into(), ask]);
    }
    let answer = last_text(input.entries, "assistant");
    if !answer.is_empty() {
        lines.extend([String::new(), "Last answer:".into(), answer]);
    }
    let files = touched_files(input.entries);
    if !files.is_empty() {
        lines.push(String::new());
        lines.push("Files touched, most recent first:".into());
        lines.extend(files.iter().map(|path| format!("- {path}")));
    }
    let model = input
        .model_label
        .filter(|label| !label.is_empty())
        .map(|label| format!(". Model: {label}"))
        .unwrap_or_default();
    lines.push(String::new());
    lines.push(format!("Working directory: {}{model}.", input.cwd));
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn entry(data: Value) -> SessionEntry {
        SessionEntry {
            id: "e".into(),
            parent_id: None,
            timestamp: String::new(),
            kind: data["type"].as_str().unwrap_or_default().into(),
            data,
        }
    }

    fn message(role: &str, content: Value) -> SessionEntry {
        entry(json!({"type": "message", "message": {"role": role, "content": content}}))
    }

    fn input(entries: &[SessionEntry]) -> HandoffBriefInput<'_> {
        HandoffBriefInput {
            entries,
            goal: None,
            cwd: "/work",
            model_label: Some("anthropic/claude"),
            previous_session_file: Some("/s/old.jsonl"),
            context_tokens: 250_400,
        }
    }

    #[test]
    fn header_and_footer_are_always_present() {
        let brief = build_handoff_brief(&input(&[]));
        assert_eq!(
            brief,
            "Handoff from an earlier session (/s/old.jsonl).\nThat session had grown to about 250k tokens, so only this brief carries over. Read the old session file if you need more detail.\n\nWorking directory: /work. Model: anthropic/claude."
        );
    }

    #[test]
    fn includes_last_turns_and_files_newest_first() {
        let entries = vec![
            message("user", json!("do the thing")),
            entry(
                json!({"type": "message", "message": {"role": "assistant", "content": [
                    {"type": "text", "text": "old"},
                    {"type": "toolCall", "name": "read", "arguments": {"path": "a.rs"}},
                    {"type": "toolCall", "name": "edit", "arguments": {"file_path": "b.rs"}},
                ]}}),
            ),
            entry(
                json!({"type": "message", "message": {"role": "assistant", "content": [
                    {"type": "text", "text": "done"},
                    {"type": "toolCall", "name": "apply_patch", "arguments": {"patch": "*** Begin Patch\n*** Update File: c.rs\n*** Add File: a.rs\n*** End Patch"}},
                ]}}),
            ),
        ];
        let brief = build_handoff_brief(&input(&entries));
        assert!(brief.contains("\nLast request:\ndo the thing\n"));
        assert!(brief.contains("\nLast answer:\ndone\n"));
        assert!(brief.contains("Files touched, most recent first:\n- c.rs\n- a.rs\n- b.rs\n"));
    }

    #[test]
    fn truncates_long_text() {
        let entries = vec![message("user", json!("x".repeat(2000)))];
        let brief = build_handoff_brief(&input(&entries));
        assert!(brief.contains(&format!("{}...", "x".repeat(MAX_TEXT_CHARS))));
    }

    #[test]
    fn caps_open_todos_with_overflow_line() {
        let tasks: Vec<Value> = (0..22)
            .map(|index| json!({"content": format!("t{index}"), "status": "pending"}))
            .collect();
        let entries = vec![entry(json!({
            "type": "custom", "customType": "senpi.todo-state",
            "data": {"schema": "v2", "phases": [{"name": "P", "tasks": tasks}]}
        }))];
        let brief = build_handoff_brief(&input(&entries));
        assert!(brief.contains("Open todos:\n- [open] P: t0\n"));
        assert!(brief.contains("- [open] P: t19\n- and 2 more\n"));
        assert!(!brief.contains("t20"));
    }
}
