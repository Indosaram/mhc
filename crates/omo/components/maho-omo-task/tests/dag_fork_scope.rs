use serde_json::json;

use maho_omo_task::dag_fork_source::{resolve_dag_fork_source, DagForkSourceResolution, ResolveDagForkSourceInput};

fn fixture_file(name: &str, header: serde_json::Value) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("dag-fork-scope-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&root).expect("create fixture directory");
    let path = root.join("session.jsonl");
    std::fs::write(&path, format!("{header}\n")).expect("write session header");
    path
}

#[test]
fn given_ordinary_startup_new_resume_or_reload_when_source_is_resolved_then_no_source_is_authorized() {
    for reason in ["startup", "new", "resume", "reload"] {
        let result = resolve_dag_fork_source(ResolveDagForkSourceInput {
            event: json!({"reason": reason, "previousSessionFile":"/parent.jsonl"}),
            current_session_id: "C".into(), current_session_file: None,
        });
        assert_eq!(result, DagForkSourceResolution::OwnOnly { diagnostic: None });
    }
}

#[test]
fn given_real_shaped_fork_event_when_identity_is_resolved_then_only_declared_immediate_parent_is_authorized() {
    let parent = fixture_file("immediate", json!({"type":"session","id":"A"}));
    let current = fixture_file("current-immediate", json!({"type":"session","id":"C","parentSession":parent.to_string_lossy()}));
    let result = resolve_dag_fork_source(ResolveDagForkSourceInput {
        event: json!({"type":"session_start","reason":"fork","previousSessionFile":parent.to_string_lossy()}),
        current_session_id: "C".into(), current_session_file: Some(current.to_string_lossy().into_owned()),
    });
    assert_eq!(result, DagForkSourceResolution::Source { session_id: "A".into() });
    std::fs::remove_dir_all(parent.parent().expect("fixture root")).expect("remove fixture");
    std::fs::remove_dir_all(current.parent().expect("fixture root")).expect("remove fixture");
}

#[test]
fn given_fork_without_offered_path_when_header_declares_parent_then_parent_is_authorized() {
    let parent = fixture_file("declared", json!({"type":"session","id":"A"}));
    let current = fixture_file("current-declared", json!({"type":"session","id":"C","parentSession":parent.to_string_lossy()}));
    let result = resolve_dag_fork_source(ResolveDagForkSourceInput {
        event: json!({"reason":"fork"}), current_session_id: "C".into(),
        current_session_file: Some(current.to_string_lossy().into_owned()),
    });
    assert_eq!(result.session_id(), Some("A"));
    std::fs::remove_dir_all(parent.parent().expect("fixture root")).expect("remove fixture");
    std::fs::remove_dir_all(current.parent().expect("fixture root")).expect("remove fixture");
}

#[test]
fn given_unrelated_offered_session_file_when_fork_is_resolved_then_it_never_authorizes_adoption() {
    let parent = fixture_file("source-unrelated", json!({"type":"session","id":"A"}));
    let unrelated = fixture_file("other-unrelated", json!({"type":"session","id":"B"}));
    let current = fixture_file("current-unrelated", json!({"type":"session","id":"C","parentSession":parent.to_string_lossy()}));
    let result = resolve_dag_fork_source(ResolveDagForkSourceInput {
        event: json!({"reason":"fork","previousSessionFile":unrelated.to_string_lossy()}),
        current_session_id: "C".into(), current_session_file: Some(current.to_string_lossy().into_owned()),
    });
    assert!(matches!(result, DagForkSourceResolution::OwnOnly { diagnostic: Some(message) } if message.contains("does not match")));
    for path in [parent, unrelated, current] { std::fs::remove_dir_all(path.parent().expect("fixture root")).expect("remove fixture"); }
}

#[test]
fn given_no_parent_or_unknown_current_file_when_fork_is_resolved_then_adoption_is_denied_with_diagnostic() {
    let no_parent = fixture_file("no-parent", json!({"type":"session","id":"C"}));
    let denied = resolve_dag_fork_source(ResolveDagForkSourceInput {
        event: json!({"reason":"fork","previousSessionFile":"/source.jsonl"}), current_session_id: "C".into(),
        current_session_file: Some(no_parent.to_string_lossy().into_owned()),
    });
    assert!(denied.diagnostic().is_some());
    let unknown = resolve_dag_fork_source(ResolveDagForkSourceInput {
        event: json!({"reason":"fork"}), current_session_id: "C".into(), current_session_file: None,
    });
    assert!(unknown.diagnostic().is_some());
    std::fs::remove_dir_all(no_parent.parent().expect("fixture root")).expect("remove fixture");
}

#[test]
fn given_absent_malformed_invalid_or_non_file_parent_provenance_when_fork_is_resolved_then_adoption_is_denied() {
    let root = std::env::temp_dir().join(format!("dag-fork-scope-invalid-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("create fixture root");
    let current = root.join("current.jsonl");
    let parent = root.join("parent.jsonl");
    for (name, bytes) in [("absent", None), ("malformed", Some(b"{broken\n".as_slice())), ("empty-id", Some(br#"{"type":"session","id":" "}"#.as_slice())), ("wrong-type", Some(br#"{"type":"message","id":"A"}"#.as_slice())), ("oversized", Some(format!(r#"{{"type":"session","id":"A","padding":"{}"}}"#, "x".repeat(128 * 1024)).as_bytes()))] {
        let _ = std::fs::remove_file(&parent);
        if let Some(bytes) = bytes { std::fs::write(&parent, bytes).expect("write parent fixture"); }
        std::fs::write(&current, json!({"type":"session","id":"C","parentSession":parent.to_string_lossy()}).to_string()).expect("write current fixture");
        let result = resolve_dag_fork_source(ResolveDagForkSourceInput { event: json!({"reason":"fork","previousSessionFile":parent.to_string_lossy()}), current_session_id:"C".into(), current_session_file:Some(current.to_string_lossy().into_owned()) });
        assert!(matches!(result, DagForkSourceResolution::OwnOnly { diagnostic:Some(_) }), "{name}");
    }
    let _ = std::fs::remove_file(&parent);
    std::fs::create_dir(&parent).expect("directory as parent");
    std::fs::write(&current, json!({"type":"session","id":"C","parentSession":parent.to_string_lossy()}).to_string()).expect("write current fixture");
    let result = resolve_dag_fork_source(ResolveDagForkSourceInput { event:json!({"reason":"fork"}), current_session_id:"C".into(), current_session_file:Some(current.to_string_lossy().into_owned()) });
    assert!(matches!(result, DagForkSourceResolution::OwnOnly { diagnostic:Some(_) }));
    std::fs::remove_dir_all(root).expect("remove fixture root");
}

#[test]
fn given_long_transcript_after_valid_header_when_fork_is_resolved_then_header_read_does_not_rewrite_transcript() {
    let root = std::env::temp_dir().join(format!("dag-fork-scope-long-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("create fixture root");
    let parent = root.join("parent.jsonl");
    let current = root.join("current.jsonl");
    std::fs::write(&parent, b"{\"type\":\"session\",\"id\":\"A\"}\n").expect("write parent");
    std::fs::write(&current, json!({"type":"session","id":"C","parentSession":parent.to_string_lossy()}).to_string()).expect("write current");
    let transcript = format!("{{\"type\":\"message\",\"text\":\"{}\"}}\n", "x".repeat(1000)).repeat(1024);
    use std::io::Write as _;
    std::fs::OpenOptions::new().append(true).open(&parent).expect("open transcript").write_all(transcript.as_bytes()).expect("append transcript");
    let before = std::fs::read(&parent).expect("read parent before");
    let result = resolve_dag_fork_source(ResolveDagForkSourceInput { event:json!({"reason":"fork","previousSessionFile":parent.to_string_lossy()}), current_session_id:"C".into(), current_session_file:Some(current.to_string_lossy().into_owned()) });
    assert_eq!(result.session_id(), Some("A"));
    assert!(before.len() > 1024 * 1024);
    assert_eq!(std::fs::read(&parent).expect("read parent after"), before);
    std::fs::remove_dir_all(root).expect("remove fixture root");
}

#[test]
fn given_fork_authorization_when_a_later_non_fork_attachment_is_resolved_then_source_is_not_retained() {
    let parent = fixture_file("no-retain-parent", json!({"type":"session","id":"A"}));
    let current = fixture_file("no-retain-current", json!({"type":"session","id":"C","parentSession":parent.to_string_lossy()}));
    assert!(resolve_dag_fork_source(ResolveDagForkSourceInput { event:json!({"reason":"fork"}), current_session_id:"C".into(), current_session_file:Some(current.to_string_lossy().into_owned()) }).session_id().is_some());
    assert_eq!(resolve_dag_fork_source(ResolveDagForkSourceInput { event:json!({"reason":"resume"}), current_session_id:"C".into(), current_session_file:Some(current.to_string_lossy().into_owned()) }), DagForkSourceResolution::OwnOnly { diagnostic:None });
    std::fs::remove_dir_all(parent.parent().expect("fixture root")).expect("remove fixture");
    std::fs::remove_dir_all(current.parent().expect("fixture root")).expect("remove fixture");
}
