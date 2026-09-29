use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::tempdir;

use super::*;
use crate::search::engine::{SearchOptions, search_transcripts};

fn header_line(id: &str, cwd: &str) -> String {
    json!({
        "type": "session",
        "version": 3,
        "id": id,
        "timestamp": "2026-08-05T17:09:02.186Z",
        "cwd": cwd
    })
    .to_string()
}

fn user_entry(id: &str, parent_id: Option<&str>, text: &str) -> String {
    json!({
        "type": "message",
        "id": id,
        "parentId": parent_id,
        "timestamp": "2026-08-05T17:09:02.340Z",
        "message": {
            "role": "user",
            "content": [{ "type": "text", "text": text }],
            "timestamp": 1785949742316_i64
        }
    })
    .to_string()
}

fn write_session(dir: &Path, name: &str, lines: &[String]) -> PathBuf {
    fs::create_dir_all(dir).expect("create dir");
    let file = dir.join(name);
    let mut content = lines.join("\n");
    content.push('\n');
    fs::write(&file, content).expect("write session file");
    file
}

#[test]
fn test_list_conversations_when_session_file_then_keyed_by_header_id() {
    let tmp = tempdir().expect("tempdir");
    let root = tmp.path();
    let project_dir = root.join("--tmp-project--");
    write_session(
        &project_dir,
        "2026-08-05T17-09-02-186Z_sess-a.jsonl",
        &[
            header_line("sess-a", "/tmp/project"),
            user_entry("e1", None, "hello world"),
        ],
    );

    let provider = SenpiSessionProvider::new(SenpiSessionProviderOptions {
        sessions_dir: root.to_path_buf(),
        excluded_dirs: None,
        hidden_marker_file: None,
        is_hidden: None,
    });

    let conversations = provider.list_conversations();
    assert_eq!(conversations.len(), 1);
    assert_eq!(conversations[0].id, "sess-a");
    let ids: Vec<String> = conversations[0]
        .messages
        .iter()
        .map(|d| d.id.clone())
        .collect();
    assert_eq!(ids, vec!["e1"]);
}

#[test]
fn test_map_content_when_user_assistant_and_tool_result_entries_then_fields_are_searchable() {
    let tmp = tempdir().expect("tempdir");
    let root = tmp.path();
    let project_dir = root.join("--tmp-project--");

    let lines = vec![
        header_line("sess-b", "/tmp/project"),
        json!({
            "type": "model_change",
            "id": "mc",
            "parentId": null,
            "timestamp": "2026-08-05T17:09:02.282Z",
            "provider": "apitopia",
            "modelId": "kimi-k3"
        })
        .to_string(),
        user_entry("u1", Some("mc"), "run the migration"),
        json!({
            "type": "message",
            "id": "a1",
            "parentId": "u1",
            "timestamp": "2026-08-05T17:09:09.088Z",
            "message": {
                "role": "assistant",
                "content": [
                    { "type": "text", "text": "starting now" },
                    { "type": "thinking", "thinking": "consider rollback safety" },
                    { "type": "toolCall", "id": "call-1", "name": "bash", "arguments": { "cmd": "psql migrate" } }
                ],
                "usage": { "input": 1, "output": 1 }
            }
        })
        .to_string(),
        json!({
            "type": "message",
            "id": "t1",
            "parentId": "a1",
            "timestamp": "2026-08-05T17:09:12.000Z",
            "message": {
                "role": "toolResult",
                "toolCallId": "call-1",
                "toolName": "bash",
                "content": [{ "type": "text", "text": "migration applied" }],
                "isError": false
            }
        })
        .to_string(),
    ];

    write_session(&project_dir, "sess-b.jsonl", &lines);

    let provider = SenpiSessionProvider::new(SenpiSessionProviderOptions {
        sessions_dir: root.to_path_buf(),
        excluded_dirs: None,
        hidden_marker_file: None,
        is_hidden: None,
    });

    let conversations = provider.list_conversations();
    assert_eq!(conversations.len(), 1);
    let documents = &conversations[0].messages;
    let doc_ids: Vec<String> = documents.iter().map(|d| d.id.clone()).collect();
    assert_eq!(doc_ids, vec!["u1", "a1", "t1"]);

    let by_id: HashMap<String, &SearchDocument> =
        documents.iter().map(|d| (d.id.clone(), d)).collect();

    assert_eq!(by_id["u1"].message_type, Some("user".to_string()));
    assert_eq!(by_id["u1"].content, Some(json!("run the migration")));

    assert_eq!(by_id["a1"].content, Some(json!("starting now")));
    assert_eq!(
        by_id["a1"].reasoning,
        Some("consider rollback safety".to_string())
    );
    assert_eq!(
        by_id["a1"].tool_calls,
        Some(vec![SearchToolCall {
            name: Some("bash".to_string()),
            arguments: Some("{\"cmd\":\"psql migrate\"}".to_string()),
        }])
    );

    assert_eq!(
        by_id["t1"].tool_calls,
        Some(vec![SearchToolCall {
            name: Some("bash".to_string()),
            arguments: None,
        }])
    );
    assert_eq!(by_id["t1"].tool_return, Some(json!("migration applied")));
    assert_eq!(
        by_id["t1"].date,
        Some("2026-08-05T17:09:12.000Z".to_string())
    );

    let res_rollback = search_transcripts(&provider, "rollback", &SearchOptions::default());
    let ids_rollback: Vec<String> = res_rollback.iter().map(|r| r.message_id.clone()).collect();
    assert_eq!(ids_rollback, vec!["a1"]);

    let res_psql = search_transcripts(&provider, "\"psql migrate\"", &SearchOptions::default());
    let ids_psql: Vec<String> = res_psql.iter().map(|r| r.message_id.clone()).collect();
    assert_eq!(ids_psql, vec!["a1"]);

    let res_applied = search_transcripts(&provider, "applied", &SearchOptions::default());
    let ids_applied: Vec<String> = res_applied.iter().map(|r| r.message_id.clone()).collect();
    assert_eq!(ids_applied, vec!["t1"]);
}

#[test]
fn test_list_conversations_when_shared_ancestor_entry_repeated_in_branches_then_deduplicated() {
    let tmp = tempdir().expect("tempdir");
    let root = tmp.path();
    let project_dir = root.join("--tmp-project--");

    let lines = vec![
        header_line("sess-c", "/tmp/project"),
        user_entry("shared", None, "shared ancestor"),
        user_entry("branch-1", Some("shared"), "first branch"),
        user_entry("shared", None, "shared ancestor"),
        user_entry("branch-2", Some("shared"), "second branch"),
    ];

    write_session(&project_dir, "sess-c.jsonl", &lines);

    let provider = SenpiSessionProvider::new(SenpiSessionProviderOptions {
        sessions_dir: root.to_path_buf(),
        excluded_dirs: None,
        hidden_marker_file: None,
        is_hidden: None,
    });

    let conversations = provider.list_conversations();
    let ids: Vec<String> = conversations[0]
        .messages
        .iter()
        .map(|d| d.id.clone())
        .collect();
    assert_eq!(ids, vec!["shared", "branch-1", "branch-2"]);
}

#[test]
fn test_read_session_file_when_malformed_line_and_missing_header_then_bad_lines_skipped_and_orphan_survives()
 {
    let tmp = tempdir().expect("tempdir");
    let root = tmp.path();
    let project_dir = root.join("--tmp-project--");

    let lines_d = vec![
        header_line("sess-d", "/tmp/project"),
        "{ this is not json".to_string(),
        user_entry("ok-1", None, "still readable"),
        "".to_string(),
        json!({ "type": "message", "id": "no-message-field", "parentId": null, "timestamp": "2026-08-05T17:09:02.340Z" }).to_string(),
        user_entry("ok-2", Some("ok-1"), "also readable"),
    ];
    write_session(&project_dir, "sess-d.jsonl", &lines_d);

    let lines_headerless = vec![user_entry("orphan", None, "no header here")];
    write_session(&project_dir, "headerless.jsonl", &lines_headerless);

    let provider = SenpiSessionProvider::new(SenpiSessionProviderOptions {
        sessions_dir: root.to_path_buf(),
        excluded_dirs: None,
        hidden_marker_file: None,
        is_hidden: None,
    });

    let conversations = provider.list_conversations();
    let with_header = conversations.iter().find(|item| item.id == "sess-d");
    let headerless = conversations.iter().find(|item| item.id == "headerless");

    assert!(with_header.is_some());
    let ids_d: Vec<String> = with_header
        .unwrap()
        .messages
        .iter()
        .map(|d| d.id.clone())
        .collect();
    assert_eq!(ids_d, vec!["ok-1", "ok-2"]);

    assert!(headerless.is_some());
    let ids_h: Vec<String> = headerless
        .unwrap()
        .messages
        .iter()
        .map(|d| d.id.clone())
        .collect();
    assert_eq!(ids_h, vec!["orphan"]);
}

#[test]
fn test_list_conversations_when_archived_sidecar_and_excluded_dir_and_is_hidden_then_hidden_and_retrievable_with_include_hidden()
 {
    let tmp = tempdir().expect("tempdir");
    let root = tmp.path();
    let project_dir = root.join("--tmp-project--");

    write_session(
        &project_dir,
        "plain.jsonl",
        &[
            header_line("plain", "/tmp"),
            user_entry("p1", None, "beta visible"),
        ],
    );
    write_session(
        &project_dir,
        "archived.jsonl",
        &[
            header_line("archived", "/tmp"),
            user_entry("a1", None, "beta archived"),
        ],
    );
    fs::write(project_dir.join("archived.jsonl.archived"), "").expect("write sidecar");

    let reflect_dir = root.join("--omo-reflection--");
    write_session(
        &reflect_dir,
        "reflect.jsonl",
        &[
            header_line("reflect", "/tmp"),
            user_entry("r1", None, "beta reflection"),
        ],
    );

    write_session(
        &project_dir,
        "task-child.jsonl",
        &[
            header_line("task-child", "/tmp"),
            user_entry("k1", None, "beta task child"),
        ],
    );

    let provider = SenpiSessionProvider::new(SenpiSessionProviderOptions {
        sessions_dir: root.to_path_buf(),
        excluded_dirs: Some(vec!["--omo-reflection--".to_string()]),
        hidden_marker_file: None,
        is_hidden: Some(Box::new(|candidate| {
            candidate.header.as_ref().map(|h| h.id.as_str()) == Some("task-child")
        })),
    });

    let conversations = provider.list_conversations();
    let visible_conv_ids: Vec<String> = conversations
        .iter()
        .filter(|item| item.hidden != Some(true))
        .map(|item| item.id.clone())
        .collect();
    assert_eq!(visible_conv_ids, vec!["plain"]);

    let visible = search_transcripts(&provider, "beta", &SearchOptions::default());
    let visible_ids: Vec<String> = visible.iter().map(|r| r.message_id.clone()).collect();
    assert_eq!(visible_ids, vec!["p1"]);

    let all = search_transcripts(
        &provider,
        "beta",
        &SearchOptions {
            include_hidden: Some(true),
            ..Default::default()
        },
    );
    let mut all_ids: Vec<String> = all.iter().map(|r| r.message_id.clone()).collect();
    all_ids.sort();
    assert_eq!(all_ids, vec!["a1", "k1", "p1", "r1"]);
}

#[test]
fn test_list_conversations_when_hidden_marker_file_inside_dir_then_all_sessions_in_dir_are_hidden()
{
    let tmp = tempdir().expect("tempdir");
    let root = tmp.path();

    let hidden_dir = root.join("--tmp-hidden--");
    write_session(
        &hidden_dir,
        "one.jsonl",
        &[
            header_line("one", "/tmp"),
            user_entry("h1", None, "beta hidden dir"),
        ],
    );
    fs::write(hidden_dir.join(".omo-hidden"), "").expect("write marker");

    let project_dir = root.join("--tmp-project--");
    write_session(
        &project_dir,
        "two.jsonl",
        &[
            header_line("two", "/tmp"),
            user_entry("v1", None, "beta plain dir"),
        ],
    );

    let provider = SenpiSessionProvider::new(SenpiSessionProviderOptions {
        sessions_dir: root.to_path_buf(),
        excluded_dirs: None,
        hidden_marker_file: Some(".omo-hidden".to_string()),
        is_hidden: None,
    });

    let results = search_transcripts(&provider, "beta", &SearchOptions::default());
    let ids: Vec<String> = results.iter().map(|r| r.message_id.clone()).collect();
    assert_eq!(ids, vec!["v1"]);
}

#[test]
fn test_list_conversations_when_missing_directory_or_non_jsonl_then_empty_and_never_panics() {
    let tmp = tempdir().expect("tempdir");
    let root = tmp.path();
    fs::write(root.join("notes.txt"), "not a session").expect("write notes");

    let missing = SenpiSessionProvider::new(SenpiSessionProviderOptions {
        sessions_dir: root.join("does-not-exist"),
        excluded_dirs: None,
        hidden_marker_file: None,
        is_hidden: None,
    });

    let empty = missing.list_conversations();
    assert_eq!(empty.is_empty(), true);

    let ignored_provider = SenpiSessionProvider::new(SenpiSessionProviderOptions {
        sessions_dir: root.to_path_buf(),
        excluded_dirs: None,
        hidden_marker_file: None,
        is_hidden: None,
    });

    let ignored = ignored_provider.list_conversations();
    assert_eq!(ignored.is_empty(), true);
}
