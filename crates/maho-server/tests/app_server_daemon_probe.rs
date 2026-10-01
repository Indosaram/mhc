use maho_server::app_server::daemon_probe::*;
use serde_json::json;
#[test]
fn initialize_requires_numeric_correlated_id_and_string_user_agent() {
    assert_eq!(
        read_initialize_probe(r#"{"id":1.0,"result":{"userAgent":"native/1"}}"#),
        Some("native/1".into())
    );
    for input in [
        "{",
        r#"{"id":"1","result":{"userAgent":"native"}}"#,
        r#"{"id":1,"result":{"userAgent":3}}"#,
        r#"{"id":2,"result":{"userAgent":"native"}}"#,
    ] {
        assert_eq!(read_initialize_probe(input), None);
    }
}
#[test]
fn settings_listen_does_not_apply_cli_port_validation() {
    let input = json!({"kind":"ws","url":"anything","host":"hostname","port":-0.5,"ignored":true});
    assert_eq!(
        parse_listen(&input),
        Some(json!({"kind":"ws","url":"anything","host":"hostname","port":-0.5}))
    );
    assert_eq!(
        parse_listen(&json!({"kind":"unix","url":"unix://","path":null})),
        Some(json!({"kind":"unix","url":"unix://"}))
    );
    assert_eq!(parse_listen(&json!({"kind":"stdio","url":"invalid"})), None);
    assert_eq!(
        running_output("already-running", None, &input, "1"),
        json!({"status":"running-unmanaged","listen":"anything","version":"1"})
    );
    assert_eq!(running_output("running", Some(7), &input, "1")["pid"], 7);
}
#[tokio::test]
async fn settings_and_cleanup_use_real_files_and_propagate_io_failures() {
    let dir = tempfile::tempdir().unwrap();
    let settings = dir.path().join("settings.json");
    let pid = dir.path().join("daemon.pid");
    let socket = dir.path().join("daemon.sock");
    assert_eq!(read_settings(&settings).await.unwrap(), None);
    tokio::fs::write(&settings, b"invalid").await.unwrap();
    assert_eq!(read_settings(&settings).await.unwrap(), None);
    let listen = json!({"kind":"unix","url":"unix://","path":socket});
    tokio::fs::write(
        &settings,
        serde_json::to_vec(&json!({"listen":listen})).unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(
        read_settings(&settings).await.unwrap(),
        Some(json!({"listen":listen}))
    );
    tokio::fs::write(&pid, b"7").await.unwrap();
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    cleanup_state(&pid, &settings, &listen).await.unwrap();
    cleanup_state(&pid, &settings, &listen).await.unwrap();
    assert!(!socket.exists());
    drop(listener);
    assert!(read_settings(dir.path()).await.is_err());
}
