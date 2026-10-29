use super::*;
use pretty_assertions::assert_eq;

fn request(envelope: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": 7, "method": "tools/call", "params": {"name": "status", "_omo": envelope}})
}

#[test]
fn valid_envelope_is_stripped_and_forwarded() {
    let message = authenticate_message(&request(auth_envelope("secret")), "secret").expect("ok");
    assert_eq!(message, AuthenticatedMessage {
        input: json!({"jsonrpc": "2.0", "id": 7, "method": "tools/call", "params": {"name": "status"}})
            .as_object()
            .cloned()
            .expect("object"),
        id: json!(7),
        method: Some("tools/call".to_string()),
    });
}

#[test]
fn wrong_token_and_missing_envelope_are_auth_errors() {
    let expected = json!({
        "jsonrpc": "2.0", "id": 7,
        "error": {"code": -32001, "message": "daemon authentication failed", "data": {"code": "daemon_authentication_failed"}}
    });
    let wrong =
        authenticate_message(&request(auth_envelope("guess")), "secret").expect_err("rejected");
    assert_eq!(wrong, expected);
    assert!(is_auth_error_response(&wrong));
    let missing =
        authenticate_message(&json!({"id": 7, "params": {}}), "secret").expect_err("rejected");
    assert_eq!(missing, expected);
    let non_record = authenticate_message(&json!([1]), "secret").expect_err("rejected");
    assert_eq!(non_record["id"], Value::Null);
}

#[test]
fn wrong_protocol_version_is_rejected_before_token_check() {
    let error = authenticate_message(
        &request(json!({"protocolVersion": 2, "token": "secret"})),
        "secret",
    )
    .expect_err("rejected");
    assert_eq!(
        error,
        json!({
            "jsonrpc": "2.0", "id": 7,
            "error": {"code": -32002, "message": "daemon protocol mismatch", "data": {"code": "daemon_protocol_mismatch"}}
        })
    );
    assert!(!is_auth_error_response(&error));
}

#[test]
fn auth_token_is_created_once_rotated_and_private() {
    let root = tempfile::tempdir().expect("tempdir");
    let paths = DaemonPaths::under_dir(root.path().join("v1"), "1");
    assert_eq!(read_auth_token(&paths), None);
    let first = read_or_create_auth_token(&paths).expect("create");
    assert_eq!(first.len(), 43);
    assert_eq!(read_or_create_auth_token(&paths).expect("reuse"), first);
    let rotated = rotate_auth_token(&paths).expect("rotate");
    assert_ne!(rotated, first);
    assert_eq!(read_auth_token(&paths), Some(rotated));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let file_mode = std::fs::metadata(&paths.auth)
            .expect("meta")
            .permissions()
            .mode()
            & 0o777;
        let dir_mode = std::fs::metadata(&paths.dir)
            .expect("meta")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!((file_mode, dir_mode), (0o600, 0o700));
    }
}

#[cfg(unix)]
#[test]
fn private_directory_rejects_symlink_file_and_foreign_owner() {
    let root = tempfile::tempdir().expect("tempdir");
    let target = root.path().join("target");
    std::fs::create_dir(&target).expect("target");
    let link = root.path().join("link");
    std::os::unix::fs::symlink(&target, &link).expect("symlink");
    let file = root.path().join("file");
    std::fs::write(&file, "x").expect("file");
    let reason = |result: io::Result<()>| unsafe_directory_reason(&result.expect_err("unsafe"));
    assert_eq!(
        reason(ensure_private_directory(&link)),
        Some(UnsafeDirectoryReason::Symlink)
    );
    assert_eq!(
        reason(ensure_private_directory(&file)),
        Some(UnsafeDirectoryReason::NotDirectory)
    );
    let other_uid = platform::current_uid().expect("uid").wrapping_add(1);
    assert_eq!(
        reason(ensure_private_directory_for(&target, Some(other_uid))),
        Some(UnsafeDirectoryReason::WrongOwner)
    );
}

#[cfg(unix)]
#[test]
fn loose_current_user_directory_is_accepted_and_made_private() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().expect("tempdir");
    let dir = root.path().join("loose");
    std::fs::create_dir(&dir).expect("dir");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    ensure_private_directory(&dir).expect("accepted");
    assert_eq!(
        std::fs::metadata(&dir).expect("meta").permissions().mode() & 0o777,
        0o700
    );
}
