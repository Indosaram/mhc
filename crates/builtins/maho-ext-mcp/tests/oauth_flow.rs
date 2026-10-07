use maho_ext_mcp::auth::{oauth::*,oauth_provider::McpOAuthProvider,token_store::McpTokenStore};
use tokio::{io::{AsyncBufReadExt,BufReader},process::Command};
#[tokio::test]
async fn real_idp_authorization_persists_tokens_and_rejects_replayed_state() {
    let mut child=Command::new("/usr/bin/node").arg("/home/indo/code/senpi/packages/coding-agent/test/mcp/fixtures/oauth-idp.ts").stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::inherit()).kill_on_drop(true).spawn().unwrap();
    let mut lines=BufReader::new(child.stdout.take().unwrap()).lines();
    let ready=tokio::time::timeout(std::time::Duration::from_secs(15),lines.next_line()).await.unwrap().unwrap().unwrap();
    let ready:serde_json::Value=serde_json::from_str(&ready).unwrap();
    let root=tempfile::tempdir().unwrap();let mut provider=McpOAuthProvider::new(McpTokenStore::new(root.path(),"fixture",ready["mcpUrl"].as_str().unwrap()));provider.require_https=false;
    provider.redirect_url=Some("http://127.0.0.1:8123/callback".into());provider.scopes=Some(vec!["mcp".into()]);
    let client=reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().unwrap();
    let begin=begin_authorization(&mut provider,&client).await.unwrap();assert!(!begin.authorized);
    let authorization=begin.authorization_url.unwrap();assert_eq!(authorization.query_pairs().find(|(key,_)|key=="code_challenge_method").unwrap().1,"S256");
    let response=client.get(authorization).send().await.unwrap();let redirect=response.headers().get("location").unwrap().to_str().unwrap().to_owned();
    complete_authorization(&mut provider,&redirect,&client).await.unwrap();
    let record=provider.store.read().unwrap().unwrap();assert!(record.access_token.unwrap().starts_with("SENTINEL_AT_"));assert!(record.refresh_token.unwrap().starts_with("SENTINEL_RT_"));
    assert!(complete_authorization(&mut provider,&redirect,&client).await.is_err());
    provider.store.update(|record|record.map(|mut record|{record.expires_at=Some(chrono::Utc::now().timestamp_millis() as f64+60000.0);record})).unwrap();
    let log_url=format!("{}/__log",ready["url"].as_str().unwrap());
    let before=client.get(&log_url).send().await.unwrap().json::<serde_json::Value>().await.unwrap()["tokenHits"].as_u64().unwrap();
    let provider=std::sync::Arc::new(provider);
    let manager=std::sync::Arc::new(maho_ext_mcp::auth::oauth_refresh::McpRefreshManager::new(provider.clone(),client.clone()));
    let results=futures::future::join_all((0..10).map(|_|{let manager=manager.clone();async move {manager.ensure_fresh().await}})).await;
    for result in results {assert!(result.unwrap().unwrap().access_token.starts_with("SENTINEL_AT_"));}
    let after=client.get(&log_url).send().await.unwrap().json::<serde_json::Value>().await.unwrap()["tokenHits"].as_u64().unwrap();assert_eq!(after-before,1);
    let logger=std::sync::Arc::new(std::sync::Mutex::new(maho_ext_mcp::log::McpLogger::new("fixture",root.path(),None).unwrap()));
    let spec=maho_ext_mcp::transport_sdk::McpTransportSpec::Http {url:url::Url::parse(ready["mcpUrl"].as_str().unwrap()).unwrap(),headers:Default::default()};
    let mcp=maho_ext_mcp::transport_sdk::McpClient::materialize("fixture",&spec,logger).await.unwrap();mcp.set_auth(manager.clone()).await;
    mcp.initialize(std::time::Duration::from_secs(5)).await.unwrap();
    let tools=mcp.request("tools/list",serde_json::json!({}),std::time::Duration::from_secs(5)).await.unwrap();assert!(!tools["tools"].as_array().unwrap().is_empty());mcp.close().await.unwrap();
    provider.store.update(|record|record.map(|mut record|{record.expires_at=Some(0.0);record.refresh_token=Some("RT_UNKNOWN".into());record})).unwrap();
    assert!(matches!(manager.ensure_fresh().await,Err(OAuthRequestError::Flow(error)) if error.oauth_kind==maho_ext_mcp::auth::oauth_errors::OAuthFailureKind::InvalidGrant));
    assert!(provider.store.read().unwrap().is_none());
    let logger=std::sync::Arc::new(std::sync::Mutex::new(maho_ext_mcp::log::McpLogger::new("missing-auth",root.path(),None).unwrap()));
    let connection=maho_ext_mcp::connection::ServerConnection::new("missing-auth",maho_ext_mcp::config_schema::McpServerConfig {enabled:Some(true),transport:Some(maho_ext_mcp::config_schema::Transport::Http),url:Some(ready["mcpUrl"].as_str().unwrap().into()),..Default::default()},None,logger);
    connection.set_auth(manager.clone());assert!(connection.connect().await.is_err());assert_eq!(connection.state(),maho_ext_mcp::connection::ServerConnectionState::NeedsAuth);connection.dispose().await.unwrap();
    provider.store.write(maho_ext_mcp::auth::token_store::McpStoredAuth {access_token:Some("AT_old".into()),refresh_token:Some("RT_TRANSIENT".into()),expires_at:Some(0.0),..Default::default()}).unwrap();
    let transient=std::sync::Arc::new({let mut other=McpOAuthProvider::new(provider.store.clone());other.client_id=Some("static-client".into());other.require_https=false;other});
    let transient_manager=maho_ext_mcp::auth::oauth_refresh::McpRefreshManager::new(transient.clone(),client.clone());
    assert!(matches!(transient_manager.ensure_fresh().await,Err(OAuthRequestError::Flow(error)) if error.oauth_kind==maho_ext_mcp::auth::oauth_errors::OAuthFailureKind::Transient));
    assert_eq!(transient.store.read().unwrap().unwrap().refresh_token.as_deref(),Some("RT_TRANSIENT"));
    let plan=maho_ext_mcp::auth::context::ServerAuthPlan {
        mode:maho_ext_mcp::auth::context::ServerAuthMode::OAuth,
        provider:Some(transient.clone()),refresh:Some(std::sync::Arc::new(transient_manager)),
    };
    let failure=plan.ensure_fresh().await.unwrap_err();
    assert_eq!(failure.kind,maho_ext_mcp::errors::McpErrorKind::Connect);
    assert!(failure.retriable);
    let paste_root=tempfile::tempdir().unwrap();let mut paste=maho_ext_mcp::auth::commands_auth::build_provider("paste",&maho_ext_mcp::config_schema::McpServerConfig {transport:Some(maho_ext_mcp::config_schema::Transport::Http),url:Some(ready["mcpUrl"].as_str().unwrap().into()),..Default::default()},paste_root.path(),Some("http://127.0.0.1:8123/callback")).unwrap();paste.require_https=false;
    let mut pending=std::collections::BTreeMap::new();let authorization=maho_ext_mcp::auth::commands_auth::run_auth_start("paste",paste,&mut pending,&client).await.unwrap();assert!(pending.contains_key("paste"));
    let redirect=client.get(authorization).send().await.unwrap().headers().get("location").unwrap().to_str().unwrap().to_owned();
    maho_ext_mcp::auth::commands_auth::run_auth_complete("paste",&redirect,&mut pending,&client).await.unwrap();assert!(pending.is_empty());assert!(maho_ext_mcp::auth::commands_auth::run_auth_complete("paste",&redirect,&mut pending,&client).await.is_err());
    let loopback_root=tempfile::tempdir().unwrap();let loopback_store=McpTokenStore::new(loopback_root.path(),"loopback",ready["mcpUrl"].as_str().unwrap());let mut loopback=McpOAuthProvider::new(loopback_store.clone());loopback.require_https=false;
    maho_ext_mcp::auth::commands_auth::run_loopback_auth(loopback,None,&client,|authorization|async {let redirect=client.get(authorization).send().await?.headers().get("location").unwrap().to_str().unwrap().to_owned();client.get(redirect).send().await?.error_for_status()?;Ok(())}).await.unwrap();assert!(loopback_store.read().unwrap().unwrap().access_token.unwrap().starts_with("SENTINEL_AT_"));
    child.kill().await.unwrap();child.wait().await.unwrap();
}
