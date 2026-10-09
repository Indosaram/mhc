use ulw_loop_core::stop_resume_hook::*;

#[test]
fn test_run_stop_resume_hook_ignores_non_stop() {
    let payload = serde_json::json!({
        "hook_event_name": "UserPromptSubmit",
        "session_id": "s1",
        "cwd": "/tmp",
        "transcript_path": "/tmp/t.jsonl",
        "stop_hook_active": false,
    });
    assert_eq!(run_stop_resume_hook(&payload), "");
}

#[test]
fn test_run_stop_resume_hook_ignores_stop_hook_active() {
    let payload = serde_json::json!({
        "hook_event_name": "Stop",
        "session_id": "s1",
        "cwd": "/tmp",
        "transcript_path": "/tmp/t.jsonl",
        "stop_hook_active": true,
    });
    assert_eq!(run_stop_resume_hook(&payload), "");
}
