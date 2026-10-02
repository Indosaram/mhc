use std::{sync::Arc,time::Duration};
use maho_ext_mcp::auth::{callback::*,oauth_errors::OAuthFailureKind};
fn options()->CallbackServerOptions {CallbackServerOptions {server_name:"s".into(),port:None,host:None,path:None,timeout:Some(Duration::from_secs(5)),validate_state:Arc::new(|state|state==Some("good"))}}
#[tokio::test]
async fn valid_redirect_delivers_code_and_closes_listener() {
    let mut channel=open_callback_channel(options(),None).await.unwrap();
    let port=url::Url::parse(&channel.redirect_url).unwrap().port().unwrap();
    let response=reqwest::get(format!("{}?code=abc123&state=good",channel.redirect_url)).await.unwrap();assert_eq!(response.status(),200);
    assert_eq!(channel.wait_for_code().await.unwrap(),CallbackResult {code:"abc123".into(),state:Some("good".into())});
    assert!(tokio::net::TcpStream::connect(("127.0.0.1",port)).await.is_err());
}
#[tokio::test]
async fn missing_state_is_rejected_even_with_permissive_validator() {
    let mut opts=options();opts.validate_state=Arc::new(|_|true);
    let mut channel=open_callback_channel(opts,None).await.unwrap();
    assert_eq!(reqwest::get(format!("{}?code=abc",channel.redirect_url)).await.unwrap().status(),400);
    assert_eq!(channel.wait_for_code().await.unwrap_err().oauth_kind,OAuthFailureKind::StateMismatch);
}
#[tokio::test]
async fn replayed_state_is_rejected() {
    let mut channel=open_callback_channel(options(),None).await.unwrap();
    assert_eq!(reqwest::get(format!("{}?code=abc&state=replayed",channel.redirect_url)).await.unwrap().status(),400);
    assert_eq!(channel.wait_for_code().await.unwrap_err().oauth_kind,OAuthFailureKind::StateMismatch);
}
#[tokio::test]
async fn occupied_fixed_port_fails_before_flow_start() {
    let listener=tokio::net::TcpListener::bind(("127.0.0.1",0)).await.unwrap();let mut opts=options();opts.port=Some(listener.local_addr().unwrap().port());
    let error=match open_callback_channel(opts,None).await {Ok(_)=>panic!("busy port accepted"),Err(e)=>e};assert!(error.to_string().contains("already in use"));
}
#[tokio::test(start_paused=true)]
async fn timeout_closes_listener() {
    let mut opts=options();opts.timeout=Some(Duration::from_secs(300));
    let mut channel=open_callback_channel(opts,None).await.unwrap();
    assert_eq!(channel.wait_for_code().await.unwrap_err().oauth_kind,OAuthFailureKind::NeedsAuth);
}
#[tokio::test]
async fn override_uses_paste_channel_without_listener() {
    let mut channel=open_callback_channel(options(),Some("https://example.test/callback")).await.unwrap();assert!(!channel.uses_loopback);
    assert_eq!(channel.wait_for_code().await.unwrap_err().oauth_kind,OAuthFailureKind::Headless);
}
#[tokio::test]
async fn unrelated_path_does_not_complete_callback() {
    let mut channel=open_callback_channel(options(),None).await.unwrap();let url=url::Url::parse(&channel.redirect_url).unwrap();
    assert_eq!(reqwest::get(url.join("/other").unwrap()).await.unwrap().status(),404);
    assert_eq!(reqwest::get(format!("{}?code=abc&state=good",channel.redirect_url)).await.unwrap().status(),200);
    assert_eq!(channel.wait_for_code().await.unwrap().code,"abc");
}
#[tokio::test]
async fn callback_server_starts_lazily_and_reuses_its_listener() {
    let mut server=McpOAuthCallbackServer::new(options());
    let url=server.start().await.unwrap();assert_eq!(server.start().await.unwrap(),url);
    assert_eq!(reqwest::get(format!("{url}?code=lazy&state=good")).await.unwrap().status(),200);
    assert_eq!(server.wait_for_code().await.unwrap().code,"lazy");server.close().await;
}
