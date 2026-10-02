use maho_ext_builtin_loose::import_repro::{parse_session_jsonl, detect_session_platform, SessionPlatform};

#[test]
fn header_validation_preserves_upstream_errors() {
    assert_eq!(parse_session_jsonl("not json").expect_err("invalid"), "first line of session file is not valid JSON");
    assert_eq!(parse_session_jsonl(r#"{"type":"session","id":"s","cwd":""}"#).expect_err("empty cwd"), "session file has no valid session header with a cwd");
    assert_eq!(parse_session_jsonl("{\"type\":\"session\",\"id\":\"s\",\"cwd\":\"/repo\"}\n{}").expect("header")["cwd"], "/repo");
}

#[test]
fn source_platform_recognizes_drive_and_msys_paths() {
    for cwd in ["C:\\repo", "d:/repo", "/c/repo"] { assert_eq!(detect_session_platform(cwd), SessionPlatform::Windows); }
    assert_eq!(detect_session_platform("/home/repo"), SessionPlatform::Unix);
    assert_eq!(detect_session_platform("repo"), SessionPlatform::Unknown);
}
