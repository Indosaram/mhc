use maho_server::app_server::websocket_auth::*;
use std::sync::{Arc, Mutex};
#[test]
fn bearer_auth_rejects_missing_wrong_case_and_blank_expected_tokens() {
    let auth = ResolvedWebSocketListenerAuth::Bearer { token:"fixture".into(), path:None };
    assert!(is_websocket_request_authorized(Some("Bearer fixture"), &auth));
    for header in [None,Some("bearer fixture"),Some("Bearer fixtures"),Some("Bearer wrongxx")] { assert!(!is_websocket_request_authorized(header, &auth)); }
    assert!(!is_websocket_request_authorized(Some("Bearer "), &ResolvedWebSocketListenerAuth::Bearer { token:String::new(), path:None }));
}
#[tokio::test]
async fn managed_token_self_heals_blank_file_and_explicit_blank_file_fails_closed() {
    let directory = tempfile::tempdir().unwrap(); let path = directory.path().join("token");
    tokio::fs::write(&path, " \n").await.unwrap();
    assert!(resolve_websocket_listener_auth(Some(WebSocketListenerAuth::TokenFile(path.clone())), None).await.is_err());
    let auth = resolve_websocket_listener_auth(None, Some(&path)).await.unwrap();
    let token = match auth { ResolvedWebSocketListenerAuth::Bearer { token, .. } => token, _ => panic!("managed token missing") };
    assert_eq!(token.len(), 64);
    assert!(token.bytes().all(|byte| byte.is_ascii_hexdigit()));
    #[cfg(unix)]
    { use std::os::unix::fs::PermissionsExt; assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600); }
    let reloaded = resolve_websocket_listener_auth(None, Some(&path)).await.unwrap();
    assert!(is_websocket_request_authorized(Some(&format!("Bearer {token}")), &reloaded));
}

#[tokio::test]
async fn token_file_trims_bom_but_preserves_next_line_character() {
    let directory = tempfile::tempdir().unwrap();let path = directory.path().join("token");
    tokio::fs::write(&path,"\u{feff}fixture\u{feff}").await.unwrap();
    let auth = resolve_websocket_listener_auth(Some(WebSocketListenerAuth::TokenFile(path.clone())),None).await.unwrap();
    assert!(is_websocket_request_authorized(Some("Bearer fixture"),&auth));
    tokio::fs::write(&path,"\u{0085}").await.unwrap();
    let auth = resolve_websocket_listener_auth(Some(WebSocketListenerAuth::TokenFile(path)),None).await.unwrap();
    assert!(is_websocket_request_authorized(Some("Bearer \u{0085}"),&auth));
}

#[tokio::test]
async fn managed_token_receipt_is_written_through_the_injected_stderr_seam() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("token");
    let captured: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let writer = captured.clone();
    let auth = resolve_websocket_listener_auth_with_receipt(None, Some(&path), Some(Arc::new(move |message: &str| writer.lock().unwrap().push(message.to_owned())))).await.unwrap();
    assert!(matches!(auth, ResolvedWebSocketListenerAuth::Bearer { .. }));
    assert_eq!(captured.lock().unwrap().as_slice(), [format!("app-server websocket token: {}", path.display())]);
    // Fail-closed empty-token file must NOT emit a success receipt.
    let empty = directory.path().join("empty");
    tokio::fs::write(&empty, " \n").await.unwrap();
    let captured_empty: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let writer_empty = captured_empty.clone();
    assert!(resolve_websocket_listener_auth_with_receipt(Some(WebSocketListenerAuth::TokenFile(empty)), None, Some(Arc::new(move |message: &str| writer_empty.lock().unwrap().push(message.to_owned())))).await.is_err());
    assert!(captured_empty.lock().unwrap().is_empty());
}
