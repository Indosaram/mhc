use maho_ext_mcp::{auth::{token_store::*,oauth_errors::*},errors::*};
use serde_json::json;
#[test]
fn token_record_permissions_and_index_are_restricted() {
    let root = tempfile::tempdir().unwrap();
    let store = McpTokenStore::new(root.path(),"server","https://example.test/mcp");
    store.write(McpStoredAuth { access_token:Some("fixture-access".into()),refresh_token:Some("fixture-refresh".into()),expires_at:Some(123.0),..Default::default() }).unwrap();
    #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; assert_eq!(std::fs::metadata(store.dir()).unwrap().permissions().mode() & 0o777,0o700); assert_eq!(std::fs::metadata(store.tokens_path()).unwrap().permissions().mode() & 0o777,0o600); }
    let index: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(store.root_dir().join("index.json")).unwrap()).unwrap();
    assert_eq!(index["server"],hash_server_url("https://example.test/mcp"));
    assert!(store.read().unwrap().unwrap().access_token.is_some());
}
#[test]
fn locked_updates_preserve_existing_fields() {
    let root = tempfile::tempdir().unwrap();
    let store = McpTokenStore::new(root.path(),"s","https://example.test");
    store.update(|_| Some(McpStoredAuth { code_verifier:Some("verifier".into()),..Default::default() })).unwrap();
    store.update(|current| { let mut next = current.unwrap_or_default(); next.resource = Some("https://example.test".into()); Some(next) }).unwrap();
    assert_eq!(store.read().unwrap().unwrap().code_verifier.as_deref(),Some("verifier"));
}
#[test]
fn traversal_names_do_not_escape_auth_directory() {
    let root = tempfile::tempdir().unwrap();
    let store = McpTokenStore::new(root.path(),"../evil","../evil");
    store.write(McpStoredAuth::default()).unwrap();
    assert_eq!(store.dir(),root.path().join("mcp-auth").join(hash_server_url("../evil")));
    assert!(!root.path().join("evil").exists());
}
#[test]
fn clear_removes_record_and_index() {
    let root = tempfile::tempdir().unwrap();
    let store = McpTokenStore::new(root.path(),"gone","https://example.test");
    store.write(McpStoredAuth::default()).unwrap();
    store.clear().unwrap();
    assert!(!store.dir().exists()); assert!(store.read().unwrap().is_none());
    let index: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(store.root_dir().join("index.json")).unwrap()).unwrap();
    assert!(index.get("gone").is_none());
}
#[test]
fn parallel_updates_are_not_lost() {
    let root = tempfile::tempdir().unwrap();
    let store = McpTokenStore::new(root.path(),"race","https://example.test");
    let threads: Vec<_> = (0..2).map(|_| { let store = store.clone(); std::thread::spawn(move || { for _ in 0..50 { store.update(|current| { let mut next = current.unwrap_or_default(); let n = next.extra.get("count").and_then(serde_json::Value::as_u64).unwrap_or(0); next.extra.insert("count".into(),json!(n+1)); Some(next) }).unwrap(); } }) }).collect();
    for thread in threads { thread.join().unwrap(); }
    assert_eq!(store.read().unwrap().unwrap().extra["count"],100);
}
#[test]
fn nested_numeric_and_text_signals_classify_retry() {
    assert!(is_retriable_mcp_error(&json!({"cause":{"response":{"statusCode":"503"}}})));
    assert!(is_retriable_mcp_error(&json!({"message":"ECONNREFUSED"})));
    assert!(!is_retriable_mcp_error(&json!({"message":"1503 bad input"})));
}
#[test]
fn numeric_status_strings_accept_javascript_radix_notation() {
    for status in ["0x1f7","0o767","0b111110111"] {assert!(is_retriable_mcp_error(&json!({"status":status})));}
    assert!(is_mcp_session_expired_error(&json!({"statusCode":"0x194"})));
    assert!(!is_retriable_mcp_error(&json!({"status":"+0x1f7"})));
}
#[test]
fn status_text_uses_ascii_javascript_word_boundaries() {
    assert!(is_retriable_mcp_error(&json!({"message":"é503é"})));
    assert!(!is_retriable_mcp_error(&json!({"message":"_503_"})));
}
#[test]
fn session_expiry_classification_distinguishes_protocol_errors() {
    assert!(is_mcp_session_expired_error(&json!({"code":-32000,"message":"Session missing"})));
    assert!(!is_mcp_session_expired_error(&json!({"code":-32000,"message":"Bad input"})));
}
#[test]
fn oauth_machine_code_overrides_incidental_text() {
    assert!(is_invalid_grant(&json!({"errorCode":"invalid_client","message":"network"})));
    assert!(!is_transient_token_error(&json!({"errorCode":"invalid_grant","message":"network 503"})));
    assert!(is_transient_token_error(&json!({"errorCode":"slow_down"})));
}
#[test]
fn provider_persists_url_bound_tokens_without_sdk_fields() {
    use maho_ext_mcp::auth::oauth_provider::*;
    let root = tempfile::tempdir().unwrap();
    let provider = McpOAuthProvider::new(McpTokenStore::new(root.path(),"s","https://example.test/mcp"));
    provider.save_tokens(&OAuthTokens { access_token:"fixture-access".into(),refresh_token:Some("fixture-refresh".into()),token_type:"Bearer".into(),expires_in:Some(60.0) },1000.0).unwrap();
    let raw: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(provider.store.tokens_path()).unwrap()).unwrap();
    assert_eq!(raw["resource"],"https://example.test/mcp"); assert!(raw.get("access_token").is_none());
    assert_eq!(provider.tokens(1000.0).unwrap().unwrap().expires_in,Some(60.0));
}
#[test]
fn token_save_without_expiry_clears_stale_expiry() {
    use maho_ext_mcp::auth::oauth_provider::*;
    let root = tempfile::tempdir().unwrap();
    let provider = McpOAuthProvider::new(McpTokenStore::new(root.path(),"s","https://example.test"));
    let mut tokens = OAuthTokens { access_token:"fixture-access".into(),refresh_token:Some("fixture-refresh".into()),token_type:"Bearer".into(),expires_in:Some(60.0) };
    provider.save_tokens(&tokens,1000.0).unwrap(); tokens.expires_in=None; tokens.refresh_token=None;
    provider.save_tokens(&tokens,2000.0).unwrap();
    let record = provider.store.read().unwrap().unwrap(); assert!(record.expires_at.is_none()); assert!(record.refresh_token.is_some());
}
#[test]
fn csrf_state_is_single_use() {
    use maho_ext_mcp::auth::oauth_provider::*;
    let root = tempfile::tempdir().unwrap();
    let mut provider = McpOAuthProvider::new(McpTokenStore::new(root.path(),"s","https://example.test"));
    let state = provider.state().unwrap();
    assert!(provider.consume_state(Some(&state))); assert!(!provider.consume_state(Some(&state)));
}
#[test]
fn saved_oauth_tokens_log_only_the_fingerprint() {
    use std::sync::{Arc,Mutex};
    let root=tempfile::tempdir().unwrap();
    let logger=Arc::new(Mutex::new(maho_ext_mcp::log::McpLogger::new("fingerprint",root.path(),None).unwrap()));
    let mut provider=maho_ext_mcp::auth::oauth_provider::McpOAuthProvider::new(maho_ext_mcp::auth::token_store::McpTokenStore::new(root.path(),"fingerprint","https://fixture.test"));provider.logger=Some(logger.clone());
    let token="synthetic-private-access";
    provider.save_tokens(&maho_ext_mcp::auth::oauth_provider::OAuthTokens {access_token:token.into(),refresh_token:None,token_type:"Bearer".into(),expires_in:None},0.0).unwrap();
    let logger=logger.lock().unwrap();let record:serde_json::Value=serde_json::from_str(&logger.get_ring_buffer()[0]).unwrap();
    assert_eq!(record["data"]["token_fp"],format!("<redacted:{}>",maho_ext_mcp::log::fingerprint_secret(&maho_ext_mcp::log::fingerprint_secret(token))));assert!(!std::fs::read_to_string(&logger.file_path).unwrap().contains(token));
}
#[tokio::test]
async fn injected_redirect_handler_runs_after_authorization_url_is_retained() {
    let root=tempfile::tempdir().unwrap();let mut provider=maho_ext_mcp::auth::oauth_provider::McpOAuthProvider::new(McpTokenStore::new(root.path(),"redirect","https://fixture.test"));
    let (sender,receiver)=tokio::sync::oneshot::channel();let sender=std::sync::Mutex::new(Some(sender));
    provider.on_redirect=Some(std::sync::Arc::new(move |url|{sender.lock().unwrap().take().unwrap().send(url).unwrap();Box::pin(async {Ok(())})}));
    let url:url::Url="https://fixture.test/authorize?state=synthetic".parse().unwrap();provider.redirect_to_authorization(url.clone()).await.unwrap();
    assert_eq!(receiver.await.unwrap(),url);assert_eq!(provider.last_authorization_url,Some(url));
}
#[test]
fn auth_dependencies_bind_provider_logger_before_refresh_clones_it() {
    use maho_ext_mcp::{auth::context::*,config_schema::*};
    let root=tempfile::tempdir().unwrap();let logger=std::sync::Arc::new(std::sync::Mutex::new(maho_ext_mcp::log::McpLogger::new("deps",root.path(),None).unwrap()));
    let config=McpServerConfig {transport:Some(Transport::Http),url:Some("https://fixture.test".into()),..Default::default()};
    let plan=resolve_server_auth_with(ServerAuthDeps {server_name:"deps",config:&config,agent_dir:Some(root.path()),logger:Some(logger.clone()),redirect_url:None,on_redirect:None,client:reqwest::Client::new()});
    assert!(std::sync::Arc::ptr_eq(plan.provider.unwrap().logger.as_ref().unwrap(),&logger));assert!(plan.refresh.is_some());
}
#[test]
fn held_auth_lock_reports_its_path_without_mutating_tokens() {
    let root=tempfile::tempdir().unwrap();let mut store=McpTokenStore::new(root.path(),"held","https://fixture.test");store.lock_retries=0;
    store.write(McpStoredAuth {code_verifier:Some("seed".into()),..Default::default()}).unwrap();
    let lock=std::fs::OpenOptions::new().read(true).write(true).open(store.lock_path()).unwrap();lock.lock().unwrap();
    let error=store.update(|_|Some(McpStoredAuth::default())).err().expect("held lock rejects updates");assert!(error.to_string().contains(&store.lock_path().display().to_string()));assert_eq!(store.read().unwrap().unwrap().code_verifier.as_deref(),Some("seed"));lock.unlock().unwrap();
}
#[test]
fn disabling_lock_allows_explicit_single_process_access() {
    let root=tempfile::tempdir().unwrap();let mut store=McpTokenStore::new(root.path(),"unlocked","https://fixture.test");store.disable_lock=true;
    store.write(McpStoredAuth {code_verifier:Some("value".into()),..Default::default()}).unwrap();assert!(!store.lock_path().exists());assert_eq!(store.read().unwrap().unwrap().code_verifier.as_deref(),Some("value"));
}
