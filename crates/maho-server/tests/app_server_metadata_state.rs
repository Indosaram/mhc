use maho_server::app_server::metadata_state::*;
use serde_json::json;
#[test]
fn git_info_merge_distinguishes_omitted_fields_and_explicit_null() {
    assert_eq!(merge_git_info(Some(&json!({"sha":"old","branch":"main","originUrl":"origin"})), &json!({"sha":null,"branch":"next","ignored":true})).unwrap(), json!({"sha":null,"branch":"next","originUrl":"origin"}));
    assert_eq!(merge_git_info(None, &json!({"sha":"first"})).unwrap(), json!({"sha":"first","branch":null,"originUrl":null}));
    assert_eq!(parse_git_info_update(&json!({"sha":" value "})).unwrap(), json!({"sha":"value"}));
    assert_eq!(parse_git_info_update(&json!({"sha":"\u{feff}value\u{feff}"})).unwrap(),json!({"sha":"value"}));
    assert_eq!(parse_git_info_update(&json!({"sha":"\u{0085}"})).unwrap(),json!({"sha":"\u{0085}"}));
    for invalid in [json!(null), json!({}), json!({"ignored":true}), json!({"sha":5}), json!({"sha":" "})] { assert!(parse_git_info_update(&invalid).is_err()); }
}

#[tokio::test]
async fn sidecar_updates_serialize_and_leave_only_owner_readable_final_file() {
    let directory = tempfile::tempdir().unwrap();
    let session = directory.path().join("session.jsonl");
    let state = ThreadMetadataState::default();
    assert!(state.read_git_info(&session).await.unwrap().is_none());
    let sha = json!({"sha":"commit"});
    let branch = json!({"branch":"main"});
    let (first, second) = tokio::join!(state.update_git_info("thread", &session, &sha), state.update_git_info("thread", &session, &branch));
    first.unwrap(); second.unwrap();
    assert_eq!(state.read_git_info(&session).await.unwrap().unwrap(), json!({"sha":"commit","branch":"main","originUrl":null}));
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(directory.path().join("session.jsonl.metadata.json")).unwrap().permissions().mode() & 0o777, 0o600);
    }
}
