use std::{collections::BTreeMap,time::Duration};
use maho_ext_mcp::auth::oauth::{request_tokens,OAuthServerInfo};
use serde_json::{Value,json};

#[tokio::test]
async fn token_endpoint_authentication_matches_server_metadata_and_registration() {
    use axum::{Router,routing::post,Json,Form,http::HeaderMap};
    let (sender,mut requests)=tokio::sync::mpsc::unbounded_channel();
    let handler=post(move |headers:HeaderMap,Form(form):Form<BTreeMap<String,String>>|{let sender=sender.clone();async move {
        sender.send((headers.get("authorization").and_then(|value|value.to_str().ok()).map(str::to_owned),form)).unwrap();
        Json(json!({"access_token":"fixture-access","token_type":"Bearer"}))
    }});
    let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();let address=listener.local_addr().unwrap();
    let (stop,stopped)=tokio::sync::oneshot::channel();
    let server=tokio::spawn(async move {axum::serve(listener,Router::new().route("/token",handler)).with_graceful_shutdown(async {let _=stopped.await;}).await.unwrap();});
    let client=reqwest::Client::new();
    for (methods,hint,expected) in [(None,None,"basic"),(Some(vec!["client_secret_post"]),None,"post"),(Some(vec!["none"]),None,"none"),(Some(vec!["client_secret_basic","client_secret_post"]),Some("client_secret_post"),"post")] {
        let mut metadata=json!({"token_endpoint":format!("http://{address}/token")});
        if let Some(methods)=methods {metadata["token_endpoint_auth_methods_supported"]=json!(methods);}
        let info=OAuthServerInfo {authorization_server_url:format!("http://{address}"),authorization_server_metadata:metadata,resource_metadata:Value::Null};
        let mut registration=json!({"client_id":"client","client_secret":"secret"});
        if let Some(hint)=hint {registration["token_endpoint_auth_method"]=json!(hint);}
        request_tokens(&client,&info,&registration,vec![("grant_type".into(),"client_credentials".into())]).await.unwrap();
        let (authorization,form)=tokio::time::timeout(Duration::from_secs(2),requests.recv()).await.unwrap().unwrap();
        match expected {
            "basic"=>{assert_eq!(authorization.as_deref(),Some("Basic Y2xpZW50OnNlY3JldA=="));assert!(!form.contains_key("client_secret"));assert!(!form.contains_key("client_id"));}
            "post"=>{assert!(authorization.is_none());assert_eq!(form["client_id"],"client");assert_eq!(form["client_secret"],"secret");}
            _=>{assert!(authorization.is_none());assert_eq!(form["client_id"],"client");assert!(!form.contains_key("client_secret"));}
        }
    }
    drop(client);stop.send(()).unwrap();tokio::time::timeout(Duration::from_secs(2),server).await.unwrap().unwrap();
}
#[tokio::test]
async fn discovery_falls_back_to_origin_without_resource_metadata() {
    use axum::{Router,routing::get,Json};
    use maho_ext_mcp::auth::{oauth::discover,oauth_provider::McpOAuthProvider,token_store::McpTokenStore};
    let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();let address=listener.local_addr().unwrap();
    let metadata=json!({"issuer":format!("http://{address}"),"authorization_endpoint":format!("http://{address}/authorize"),"token_endpoint":format!("http://{address}/token"),"response_types_supported":["code"]});
    let expected=metadata.clone();
    let (stop,stopped)=tokio::sync::oneshot::channel();
    let server=tokio::spawn(async move {axum::serve(listener,Router::new().route("/.well-known/openid-configuration",get(move ||{let metadata=metadata.clone();async move {Json(metadata)}}))).with_graceful_shutdown(async {let _=stopped.await;}).await.unwrap();});
    let root=tempfile::tempdir().unwrap();let provider=McpOAuthProvider::new(McpTokenStore::new(root.path(),"fallback",&format!("http://{address}/mcp")));
    let client=reqwest::Client::new();let info=discover(&provider,&client).await.unwrap();
    assert_eq!(info.authorization_server_url,format!("http://{address}/"));assert_eq!(info.authorization_server_metadata,expected);assert!(info.resource_metadata.is_null());
    drop(client);stop.send(()).unwrap();tokio::time::timeout(Duration::from_secs(2),server).await.unwrap().unwrap();
}
