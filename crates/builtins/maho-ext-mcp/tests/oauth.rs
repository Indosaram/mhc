use maho_ext_mcp::auth::{oauth::*,oauth_refresh::*,token_store::McpStoredAuth,oauth_errors::OAuthFailureKind};
use serde_json::json;
#[test]
fn expiry_leeway_includes_exact_boundary() {
    let mut record=McpStoredAuth {access_token:Some("fixture".into()),expires_at:Some(300000.0),..Default::default()};assert!(is_token_stale(Some(&record),0.0));record.expires_at=Some(300001.0);assert!(!is_token_stale(Some(&record),0.0));record.expires_at=None;assert!(!is_token_stale(Some(&record),0.0));record.access_token=None;assert!(is_token_stale(Some(&record),0.0));
}
#[test]
fn proof_key_support_must_be_explicit() {
    assert!(assert_s256_supported(Some(&json!({"code_challenge_methods_supported":["S256"]})),"srv").is_ok());
    for metadata in [json!({}),json!({"code_challenge_methods_supported":["plain"]})] {assert_eq!(assert_s256_supported(Some(&metadata),"srv").unwrap_err().oauth_kind,OAuthFailureKind::S256Unsupported);}
}
#[test]
fn redirect_parsing_preserves_first_query_value() {
    assert_eq!(parse_redirect(" http://127.0.0.1/callback?code=a%2Bb&code=other&state=csrf ","srv").unwrap(),("a+b".into(),Some("csrf".into())));
}
#[test]
fn malformed_failed_and_empty_redirects_are_rejected() {
    for input in ["not a url","http://127.0.0.1/callback?code=","http://127.0.0.1/callback?error=access_denied&code=abc"] {assert_eq!(parse_redirect(input,"srv").unwrap_err().oauth_kind,OAuthFailureKind::NeedsAuth);}
}

#[tokio::test]
async fn refresh_returns_fresh_tokens_without_discovery_or_registration() {
    use maho_ext_mcp::auth::{oauth_provider::McpOAuthProvider,token_store::McpTokenStore};
    let root=tempfile::tempdir().unwrap();
    let store=McpTokenStore::new(root.path(),"fresh","invalid discovery URL");
    store.write(McpStoredAuth {access_token:Some("fixture-fresh".into()),..Default::default()}).unwrap();
    let manager=McpRefreshManager::new(std::sync::Arc::new(McpOAuthProvider::new(store)),reqwest::Client::new());
    assert_eq!(manager.refresh().await.unwrap().access_token,"fixture-fresh");
}

#[tokio::test]
async fn missing_refresh_token_is_reported_before_discovery() {
    use maho_ext_mcp::auth::{oauth_provider::McpOAuthProvider,token_store::McpTokenStore};
    let root=tempfile::tempdir().unwrap();
    let store=McpTokenStore::new(root.path(),"missing","invalid discovery URL");
    store.write(McpStoredAuth {access_token:Some("fixture-stale".into()),expires_at:Some(0.0),..Default::default()}).unwrap();
    let manager=McpRefreshManager::new(std::sync::Arc::new(McpOAuthProvider::new(store)),reqwest::Client::new());
    assert!(matches!(manager.refresh().await,Err(OAuthRequestError::Flow(error)) if error.oauth_kind==OAuthFailureKind::NeedsAuth));
}
#[tokio::test]
async fn refresh_does_not_retry_unclassified_token_errors() {
    use std::sync::{Arc,atomic::{AtomicUsize,Ordering}};
    use maho_ext_mcp::auth::{oauth_provider::McpOAuthProvider,token_store::McpTokenStore};
    let hits=Arc::new(AtomicUsize::new(0));let observed=hits.clone();
    let app=axum::Router::new().route("/token",axum::routing::post(move||{let hits=observed.clone();async move {hits.fetch_add(1,Ordering::SeqCst);(axum::http::StatusCode::BAD_REQUEST,axum::Json(json!({"error":"invalid_request"})))}}));
    let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();let base=format!("http://{}",listener.local_addr().unwrap());
    let (shutdown,closed)=tokio::sync::oneshot::channel();
    let server=tokio::spawn(async move {axum::serve(listener,app).with_graceful_shutdown(async {let _=closed.await;}).await.unwrap();});
    let root=tempfile::tempdir().unwrap();let store=McpTokenStore::new(root.path(),"nontransient",&base);
    store.write(McpStoredAuth {access_token:Some("fixture-stale".into()),refresh_token:Some("fixture-refresh".into()),expires_at:Some(0.0),discovery_state:Some(json!({"authorizationServerUrl":base,"authorizationServerMetadata":{"token_endpoint":format!("{base}/token")},"resourceMetadata":null})),..Default::default()}).unwrap();
    let mut provider=McpOAuthProvider::new(store.clone());provider.client_id=Some("fixture-client".into());
    let info=OAuthServerInfo {authorization_server_url:base.clone(),authorization_server_metadata:json!({"token_endpoint":format!("{base}/token")}),resource_metadata:serde_json::Value::Null};
    let mut manager=McpRefreshManager::new_with_options(Arc::new(provider),reqwest::Client::new(),RefreshManagerOptions {discover:Some(Arc::new(move |_| {let info=info.clone();Box::pin(async move {Ok(info)})})),..Default::default()});manager.retry_delay=std::time::Duration::ZERO;
    let result=manager.refresh().await;shutdown.send(()).unwrap();server.await.unwrap();
    assert!(matches!(result,Err(OAuthRequestError::Flow(error)) if error.oauth_kind==OAuthFailureKind::Transient));
    assert_eq!(hits.load(Ordering::SeqCst),1);
    assert_eq!(store.read().unwrap().unwrap().refresh_token.as_deref(),Some("fixture-refresh"));
}
#[tokio::test]
async fn concurrent_failed_refreshes_share_one_request_and_later_calls_retry() {
    use std::sync::{Arc,atomic::{AtomicUsize,Ordering}};
    use maho_ext_mcp::auth::{oauth_provider::McpOAuthProvider,token_store::McpTokenStore};
    let hits=Arc::new(AtomicUsize::new(0));let observed=hits.clone();
    let release=Arc::new(tokio::sync::Semaphore::new(0));let gate=release.clone();
    let (entered,mut requests)=tokio::sync::mpsc::unbounded_channel();
    let app=axum::Router::new().route("/token",axum::routing::post(move||{let hits=observed.clone();let gate=gate.clone();let entered=entered.clone();async move {
        hits.fetch_add(1,Ordering::SeqCst);entered.send(()).unwrap();gate.acquire().await.unwrap().forget();
        (axum::http::StatusCode::BAD_REQUEST,axum::Json(json!({"error":"invalid_request"})))
    }}));
    let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();let base=format!("http://{}",listener.local_addr().unwrap());
    let (shutdown,closed)=tokio::sync::oneshot::channel();let server=tokio::spawn(async move {axum::serve(listener,app).with_graceful_shutdown(async {let _=closed.await;}).await.unwrap();});
    let root=tempfile::tempdir().unwrap();let store=McpTokenStore::new(root.path(),"coalesce",&base);
    store.write(McpStoredAuth {access_token:Some("fixture-stale".into()),refresh_token:Some("fixture-refresh".into()),expires_at:Some(0.0),discovery_state:Some(json!({"authorizationServerUrl":base,"authorizationServerMetadata":{"token_endpoint":format!("{base}/token")},"resourceMetadata":null})),..Default::default()}).unwrap();
    let mut provider=McpOAuthProvider::new(store);provider.client_id=Some("fixture-client".into());let info=OAuthServerInfo {authorization_server_url:base.clone(),authorization_server_metadata:json!({"token_endpoint":format!("{base}/token")}),resource_metadata:serde_json::Value::Null};let manager=Arc::new(McpRefreshManager::new_with_options(Arc::new(provider),reqwest::Client::new(),RefreshManagerOptions {discover:Some(Arc::new(move |_| {let info=info.clone();Box::pin(async move {Ok(info)})})),..Default::default()}));
    let mut first=Box::pin(manager.refresh());let mut second=Box::pin(manager.refresh());
    assert!(futures::poll!(&mut first).is_pending());
    tokio::time::timeout(std::time::Duration::from_secs(2),requests.recv()).await.unwrap().unwrap();
    assert!(futures::poll!(&mut second).is_pending());release.add_permits(2);
    let (first,second)=tokio::join!(first,second);let concurrent_hits=hits.load(Ordering::SeqCst);
    release.add_permits(1);let later=manager.refresh().await;
    shutdown.send(()).unwrap();server.await.unwrap();
    for result in [first,second,later] {assert!(matches!(result,Err(OAuthRequestError::Flow(error)) if error.oauth_kind==OAuthFailureKind::Transient));}
    assert_eq!(concurrent_hits,1);assert_eq!(hits.load(Ordering::SeqCst),2);
}
#[tokio::test]
async fn injected_discover_replaces_sdk_discovery_on_the_cache_miss_path() {
    use std::sync::{Arc,atomic::{AtomicUsize,Ordering}};
    use maho_ext_mcp::auth::{oauth_errors::OAuthFlowError,oauth_provider::McpOAuthProvider,token_store::McpTokenStore};
    let root=tempfile::tempdir().unwrap();
    let store=McpTokenStore::new(root.path(),"injected","https://mcp.invalid/sse");
    store.write(McpStoredAuth {access_token:Some("stale".into()),refresh_token:Some("refresh".into()),expires_at:Some(0.0),..Default::default()}).unwrap();
    let hits=Arc::new(AtomicUsize::new(0));let observed=hits.clone();
    let discover:DiscoverFn=Arc::new(move |url| {let observed=observed.clone();let url=url.to_owned();Box::pin(async move {observed.fetch_add(1,Ordering::SeqCst);assert_eq!(url,"https://mcp.invalid/sse");Err(OAuthRequestError::Flow(Box::new(OAuthFlowError::new(OAuthFailureKind::NeedsAuth,"injected discovery"))))})});
    let manager=McpRefreshManager::new_with_options(Arc::new(McpOAuthProvider::new(store)),reqwest::Client::new(),RefreshManagerOptions {discover:Some(discover),..Default::default()});
    let result=manager.refresh().await;
    assert!(matches!(&result,Err(OAuthRequestError::Flow(error)) if error.oauth_kind==OAuthFailureKind::NeedsAuth));
    assert_eq!(hits.load(Ordering::SeqCst),1,"the injected discover must run on a cache miss");
}
#[tokio::test]
async fn injected_discovery_overrides_stored_state_during_refresh() {
    use std::sync::{Arc,atomic::{AtomicUsize,Ordering}};
    use maho_ext_mcp::auth::{oauth_errors::OAuthFlowError,oauth_provider::McpOAuthProvider,token_store::McpTokenStore};
    let root=tempfile::tempdir().unwrap();
    let store=McpTokenStore::new(root.path(),"cached","https://mcp.invalid/sse");
    store.write(McpStoredAuth {access_token:Some("stale".into()),refresh_token:Some("refresh".into()),expires_at:Some(0.0),discovery_state:Some(json!({"authorizationServerUrl":"https://auth.invalid","authorizationServerMetadata":{"token_endpoint":"https://auth.invalid/token"},"resourceMetadata":null})),..Default::default()}).unwrap();
    let hits=Arc::new(AtomicUsize::new(0));let observed=hits.clone();
    let discover:DiscoverFn=Arc::new(move |_url| {let observed=observed.clone();Box::pin(async move {observed.fetch_add(1,Ordering::SeqCst);Err(OAuthRequestError::Flow(Box::new(OAuthFlowError::new(OAuthFailureKind::NeedsAuth,"must not run"))))})});
    let manager=McpRefreshManager::new_with_options(Arc::new(McpOAuthProvider::new(store)),reqwest::Client::new(),RefreshManagerOptions {discover:Some(discover),..Default::default()});
    let result=manager.refresh().await;
    assert!(matches!(&result,Err(OAuthRequestError::Flow(error)) if error.oauth_kind==OAuthFailureKind::NeedsAuth));
    assert_eq!(hits.load(Ordering::SeqCst),1);
}
