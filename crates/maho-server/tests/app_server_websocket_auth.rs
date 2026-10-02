use maho_server::app_server::websocket_auth::*;
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
