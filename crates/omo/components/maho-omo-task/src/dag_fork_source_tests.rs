use serde_json::json;

use super::{
    DagForkSourceResolution, ResolveDagForkSourceInput, parse_dag_session_start_event,
    read_dag_session_header, resolve_dag_fork_source,
};

fn temp_file(name: &str, contents: &[u8]) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("dag-fork-source-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&root).expect("create fixture directory");
    let path = root.join("session.jsonl");
    std::fs::write(&path, contents).expect("write fixture file");
    path
}

#[test]
fn given_malformed_start_event_when_parsed_then_rejected() {
    for event in [
        json!(null),
        json!("fork"),
        json!([]),
        json!({}),
        json!({"reason":" "}),
    ] {
        assert!(parse_dag_session_start_event(&event).is_none());
    }
}

#[test]
fn given_fork_payload_with_blank_previous_session_file_when_parsed_then_file_is_absent() {
    assert_eq!(
        parse_dag_session_start_event(&json!({"reason":"fork","previousSessionFile":"  "})),
        Some(super::DagSessionStartEvent {
            reason: "fork".into(),
            previous_session_file: None
        })
    );
}

#[test]
fn given_non_fork_reason_when_resolved_then_own_only_without_diagnostic() {
    assert_eq!(
        resolve_dag_fork_source(ResolveDagForkSourceInput {
            event: json!({"reason":"resume"}),
            current_session_id: "child".into(),
            current_session_file: None,
        }),
        DagForkSourceResolution::OwnOnly { diagnostic: None }
    );
}

#[test]
fn given_fork_header_declaring_parent_when_resolved_then_source_identity_returned() {
    let parent = temp_file("parent", br#"{"type":"session","id":"parent"}"#);
    let current_header =
        json!({"type":"session","id":"child","parentSession":parent.to_string_lossy()});
    let current = temp_file("current", current_header.to_string().as_bytes());
    let result = resolve_dag_fork_source(ResolveDagForkSourceInput {
        event: json!({"reason":"fork","previousSessionFile":parent.to_string_lossy()}),
        current_session_id: "child".into(),
        current_session_file: Some(current.to_string_lossy().into_owned()),
    });
    assert_eq!(
        result,
        DagForkSourceResolution::Source {
            session_id: "parent".into()
        }
    );
    std::fs::remove_dir_all(parent.parent().expect("fixture parent")).expect("remove fixture");
    std::fs::remove_dir_all(current.parent().expect("fixture parent")).expect("remove fixture");
}

#[test]
fn given_offered_path_not_matching_declared_parent_when_resolved_then_source_is_rejected() {
    let parent = temp_file("parent-mismatch", br#"{"type":"session","id":"parent"}"#);
    let other = temp_file("other-mismatch", br#"{"type":"session","id":"other"}"#);
    let current_header =
        json!({"type":"session","id":"child","parentSession":parent.to_string_lossy()});
    let current = temp_file("current-mismatch", current_header.to_string().as_bytes());
    let result = resolve_dag_fork_source(ResolveDagForkSourceInput {
        event: json!({"reason":"fork","previousSessionFile":other.to_string_lossy()}),
        current_session_id: "child".into(),
        current_session_file: Some(current.to_string_lossy().into_owned()),
    });
    assert!(
        matches!(result, DagForkSourceResolution::OwnOnly { diagnostic: Some(message) } if message.contains("does not match"))
    );
    for path in [parent, other, current] {
        std::fs::remove_dir_all(path.parent().expect("fixture parent")).expect("remove fixture");
    }
}

#[test]
fn given_header_without_trailing_newline_when_read_then_identity_is_returned() {
    let path = temp_file("newline", br#"{"type":"session","id":"session-id"}"#);
    assert_eq!(
        read_dag_session_header(&path).expect("read header").id,
        "session-id"
    );
    std::fs::remove_dir_all(path.parent().expect("fixture parent")).expect("remove fixture");
}

#[test]
fn given_header_larger_than_read_limit_when_read_then_it_is_rejected() {
    let path = temp_file(
        "oversized-direct",
        format!(
            r#"{{"type":"session","id":"A","padding":"{}"}}"#,
            "x".repeat(70 * 1024)
        )
        .as_bytes(),
    );
    assert!(
        read_dag_session_header(&path)
            .expect_err("oversized header")
            .contains("read limit")
    );
    std::fs::remove_dir_all(path.parent().expect("fixture parent")).expect("remove fixture");
}

#[test]
fn given_fork_session_declares_no_parent_when_resolved_then_own_only_with_diagnostic() {
    let current = temp_file("no-parent", br#"{"type":"session","id":"child"}"#);
    let result = resolve_dag_fork_source(ResolveDagForkSourceInput {
        event: json!({"reason":"fork"}),
        current_session_id: "child".into(),
        current_session_file: Some(current.to_string_lossy().into_owned()),
    });
    assert!(
        matches!(result, DagForkSourceResolution::OwnOnly { diagnostic: Some(message) } if message.contains("declares no parent"))
    );
    std::fs::remove_dir_all(current.parent().expect("fixture parent")).expect("remove fixture");
}

#[test]
fn given_unreadable_fork_source_when_resolved_then_own_only_with_diagnostic() {
    let current = temp_file(
        "unreadable-source",
        br#"{"type":"session","id":"child","parentSession":"/missing/fork-parent.jsonl"}"#,
    );
    let result = resolve_dag_fork_source(ResolveDagForkSourceInput {
        event: json!({"reason":"fork"}),
        current_session_id: "child".into(),
        current_session_file: Some(current.to_string_lossy().into_owned()),
    });
    assert!(
        matches!(result, DagForkSourceResolution::OwnOnly { diagnostic: Some(message) } if message.contains("fork source unreadable"))
    );
    std::fs::remove_dir_all(current.parent().expect("fixture parent")).expect("remove fixture");
}

#[test]
fn given_blank_parent_session_when_read_then_no_parent_is_declared() {
    let path = temp_file(
        "blank-parent",
        br#"{"type":"session","id":"A","parentSession":" "}"#,
    );
    assert_eq!(
        read_dag_session_header(&path)
            .expect("read header")
            .parent_session,
        None
    );
    std::fs::remove_dir_all(path.parent().expect("fixture parent")).expect("remove fixture");
}
