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

#[test]
fn source_reference_grammar_distinguishes_local_gist_and_issue() {
    use maho_ext_builtin_loose::import_repro::{parse_ref,SessionReference};
    use std::path::Path;
    let id="b4d100022aefb12f25dd2d8485e0a82a";
    for reference in [id.to_owned(),format!("https://gist.github.com/user/{id}?x=1"),format!("https://gist.github.com/{id}"),format!("https://pi.dev/session/#{id}")] {
        assert_eq!(parse_ref(&reference,Path::new("/repo")).expect("gist"),SessionReference::Gist {id:id.into()});
    }
    assert_eq!(parse_ref("https://github.com/owner/repo/issues/123#comment",Path::new("/repo")).expect("issue"),SessionReference::Issue {owner:"owner".into(),repo:"repo".into(),issue:"123".into()});
    assert_eq!(parse_ref("nested/../session.jsonl",Path::new("/repo")).expect("file"),SessionReference::File {path:"/repo/session.jsonl".into()});
    for reference in ["abc","http://gist.github.com/123","https://github.com/owner/repo/issues/nope"] {assert!(parse_ref(reference,Path::new("/repo")).is_err());}
}

#[test]
fn cwd_rewrite_preserves_json_escaping_and_windows_variants() {
    use maho_ext_builtin_loose::import_repro::rewrite_session_cwd;
    let raw=serde_json::json!({"cwd":"C:\\work\\repo","paths":["C:/work/repo/file","/c/work/repo/file","/C/work/repo/file"],"other":"/another"}).to_string();
    let rewritten=rewrite_session_cwd(&raw,"C:\\work\\repo\\","/local/repo");
    let value:serde_json::Value=serde_json::from_str(&rewritten).expect("valid JSON");
    assert_eq!(value,serde_json::json!({"cwd":"/local/repo","paths":["/local/repo/file","/local/repo/file","/local/repo/file"],"other":"/another"}));
    let name="pi-ci-0123456789abcdef0123456789abcdef";
    let raw=serde_json::json!({"path":format!("D:\\alternate\\{name}\\file")}).to_string();
    let value:serde_json::Value=serde_json::from_str(&rewrite_session_cwd(&raw,&format!("/tmp/{name}"),"/local")).expect("valid JSON");
    assert_eq!(value["path"],"/local\\file");
}

#[test]
fn exported_html_recovers_header_entries_and_final_newline() {
    use base64::{Engine,engine::general_purpose::STANDARD};
    use maho_ext_builtin_loose::import_repro::decode_exported_html;
    let header=serde_json::json!({"type":"session","id":"s","cwd":"/repo"});
    let entry=serde_json::json!({"type":"message","id":"m"});
    let encoded=STANDARD.encode(serde_json::json!({"header":header,"entries":[entry]}).to_string());
    let html=format!("<script id=\"session-data\" type=\"application/json\">{encoded}</script>");
    let (actual,jsonl)=decode_exported_html(&html).expect("session data");
    assert_eq!(actual,header);assert!(jsonl.ends_with('\n'));
    assert_eq!(jsonl.lines().map(|line|serde_json::from_str::<serde_json::Value>(line).expect("entry")).collect::<Vec<_>>(),[header,entry]);
    assert!(decode_exported_html("<html></html>").is_err());
}
