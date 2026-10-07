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
async fn discovery_retries_the_origin_authorization_server_metadata() {
    use axum::{Router,routing::get,Json};
    use maho_ext_mcp::auth::{oauth::discover,oauth_provider::McpOAuthProvider,token_store::McpTokenStore};
    let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();let address=listener.local_addr().unwrap();
    let metadata=json!({"issuer":format!("http://{address}"),"authorization_endpoint":format!("http://{address}/authorize"),"token_endpoint":format!("http://{address}/token"),"response_types_supported":["code"]});
    let expected=metadata.clone();
    let (stop,stopped)=tokio::sync::oneshot::channel();
    // Only the origin form is served: the path-suffixed RFC 8414 form 404s, so a success here
    // proves the pinned `fetchAuthorizationServerMetadata` origin retry ran.
    let server=tokio::spawn(async move {axum::serve(listener,Router::new().route("/.well-known/oauth-authorization-server",get(move ||{let metadata=metadata.clone();async move {Json(metadata)}}))).with_graceful_shutdown(async {let _=stopped.await;}).await.unwrap();});
    let root=tempfile::tempdir().unwrap();let mut provider=McpOAuthProvider::new(McpTokenStore::new(root.path(),"fallback",&format!("http://{address}/mcp")));provider.require_https=false;
    let client=reqwest::Client::new();let info=discover(&provider,&client).await.unwrap();
    assert_eq!(info.authorization_server_url,format!("http://{address}/mcp"));assert_eq!(info.authorization_server_metadata,expected);assert!(info.resource_metadata.is_null());
    drop(client);stop.send(()).unwrap();tokio::time::timeout(Duration::from_secs(2),server).await.unwrap().unwrap();
}

#[tokio::test]
async fn discovery_probes_the_origin_protected_resource_path() {
    use axum::{Router,routing::get,Json};
    use maho_ext_mcp::auth::{oauth::discover,oauth_provider::McpOAuthProvider,token_store::McpTokenStore};
    let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();let address=listener.local_addr().unwrap();
    let protected=json!({"resource":format!("http://{address}/tenant/mcp"),"authorization_servers":[format!("http://{address}")]});
    let metadata=json!({"issuer":format!("http://{address}"),"authorization_endpoint":format!("http://{address}/authorize"),"token_endpoint":format!("http://{address}/token")});
    let (stop,stopped)=tokio::sync::oneshot::channel();
    // The resource URL carries a path, but the pinned `new URL("/.well-known/...", resource)`
    // resolves against the origin, so only the origin routes exist.
    let app=Router::new()
        .route("/.well-known/oauth-protected-resource",get(move ||{let protected=protected.clone();async move {Json(protected)}}))
        .route("/.well-known/oauth-authorization-server",get(move ||{let metadata=metadata.clone();async move {Json(metadata)}}));
    let server=tokio::spawn(async move {axum::serve(listener,app).with_graceful_shutdown(async {let _=stopped.await;}).await.unwrap();});
    let root=tempfile::tempdir().unwrap();let mut provider=McpOAuthProvider::new(McpTokenStore::new(root.path(),"origin",&format!("http://{address}/tenant/mcp")));provider.require_https=false;
    let client=reqwest::Client::new();let info=discover(&provider,&client).await.unwrap();
    assert_eq!(info.authorization_server_url,format!("http://{address}/"));assert_eq!(info.resource_metadata["authorization_servers"][0],json!(format!("http://{address}")));
    drop(client);stop.send(()).unwrap();tokio::time::timeout(Duration::from_secs(2),server).await.unwrap().unwrap();
}

#[tokio::test]
async fn discovery_requires_authorization_servers_in_protected_resource_metadata() {
    use axum::{Router,routing::get,Json};
    use maho_ext_mcp::auth::{oauth::discover,oauth_provider::McpOAuthProvider,token_store::McpTokenStore};
    let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();let address=listener.local_addr().unwrap();
    let (stop,stopped)=tokio::sync::oneshot::channel();
    let server=tokio::spawn(async move {axum::serve(listener,Router::new().route("/.well-known/oauth-protected-resource",get(||async {Json(json!({"resource":"https://mcp.example/mcp"}))}))).with_graceful_shutdown(async {let _=stopped.await;}).await.unwrap();});
    let root=tempfile::tempdir().unwrap();let mut provider=McpOAuthProvider::new(McpTokenStore::new(root.path(),"missing-servers",&format!("http://{address}/mcp")));provider.require_https=false;
    let client=reqwest::Client::new();let error=discover(&provider,&client).await.unwrap_err();
    assert_eq!(error.to_string(),"OAuth protected resource metadata missing authorization_servers");
    drop(client);stop.send(()).unwrap();tokio::time::timeout(Duration::from_secs(2),server).await.unwrap().unwrap();
}

#[tokio::test]
async fn discovery_reports_the_protected_resource_status() {
    use axum::{Router,routing::get,http::StatusCode,Json};
    use maho_ext_mcp::auth::{oauth::discover,oauth_provider::McpOAuthProvider,token_store::McpTokenStore};
    let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();let address=listener.local_addr().unwrap();
    let (stop,stopped)=tokio::sync::oneshot::channel();
    let server=tokio::spawn(async move {axum::serve(listener,Router::new().route("/.well-known/oauth-protected-resource",get(||async {(StatusCode::INTERNAL_SERVER_ERROR,Json(json!({"error":"boom"}))) }))).with_graceful_shutdown(async {let _=stopped.await;}).await.unwrap();});
    let root=tempfile::tempdir().unwrap();let mut provider=McpOAuthProvider::new(McpTokenStore::new(root.path(),"prm-status",&format!("http://{address}/mcp")));provider.require_https=false;
    let client=reqwest::Client::new();let error=discover(&provider,&client).await.unwrap_err();
    assert_eq!(error.to_string(),"OAuth protected resource metadata fetch failed (500)");
    drop(client);stop.send(()).unwrap();tokio::time::timeout(Duration::from_secs(2),server).await.unwrap().unwrap();
}

#[tokio::test]
async fn discovery_requires_the_authorization_and_token_endpoints() {
    use axum::{Router,routing::get,Json};
    use maho_ext_mcp::auth::{oauth::discover,oauth_provider::McpOAuthProvider,token_store::McpTokenStore};
    let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();let address=listener.local_addr().unwrap();
    let protected=json!({"resource":format!("http://{address}/mcp"),"authorization_servers":[format!("http://{address}")]});
    let metadata=json!({"issuer":format!("http://{address}"),"authorization_endpoint":format!("http://{address}/authorize")});
    let (stop,stopped)=tokio::sync::oneshot::channel();
    let app=Router::new()
        .route("/.well-known/oauth-protected-resource",get(move ||{let protected=protected.clone();async move {Json(protected)}}))
        .route("/.well-known/oauth-authorization-server",get(move ||{let metadata=metadata.clone();async move {Json(metadata)}}));
    let server=tokio::spawn(async move {axum::serve(listener,app).with_graceful_shutdown(async {let _=stopped.await;}).await.unwrap();});
    let root=tempfile::tempdir().unwrap();let mut provider=McpOAuthProvider::new(McpTokenStore::new(root.path(),"missing-endpoint",&format!("http://{address}/mcp")));provider.require_https=false;
    let client=reqwest::Client::new();let error=discover(&provider,&client).await.unwrap_err();
    assert_eq!(error.to_string(),"OAuth metadata missing token_endpoint");
    drop(client);stop.send(()).unwrap();tokio::time::timeout(Duration::from_secs(2),server).await.unwrap().unwrap();
}

#[test]
fn resource_indicator_normalizes_the_server_url() {
    use maho_ext_mcp::auth::{oauth_provider::McpOAuthProvider,token_store::McpTokenStore};
    let root=tempfile::tempdir().unwrap();
    let bare=McpOAuthProvider::new(McpTokenStore::new(root.path(),"bare","https://example.test"));
    let pathed=McpOAuthProvider::new(McpTokenStore::new(root.path(),"pathed","https://example.test/mcp"));
    assert_eq!(bare.resource_indicator(),"https://example.test/");
    assert_eq!(pathed.resource_indicator(),"https://example.test/mcp");
}

#[tokio::test]
async fn dynamic_client_registration_body_matches_the_pinned_shape() {
    use axum::{Router,routing::{get,post},Json};
    use maho_ext_mcp::auth::{oauth::begin_authorization,oauth_provider::McpOAuthProvider,token_store::McpTokenStore};
    let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();let address=listener.local_addr().unwrap();
    let (sender,mut registrations)=tokio::sync::mpsc::unbounded_channel();
    let protected=json!({"resource":format!("http://{address}/mcp"),"authorization_servers":[format!("http://{address}")]});
    let metadata=json!({"issuer":format!("http://{address}"),"authorization_endpoint":format!("http://{address}/authorize"),"token_endpoint":format!("http://{address}/token"),"registration_endpoint":format!("http://{address}/register"),"code_challenge_methods_supported":["S256"]});
    let register=post(move |Json(body):Json<serde_json::Value>|{let sender=sender.clone();async move {sender.send(body).unwrap();Json(json!({"client_id":"registered-client"}))}});
    let (stop,stopped)=tokio::sync::oneshot::channel();
    let app=Router::new()
        .route("/.well-known/oauth-protected-resource",get(move ||{let protected=protected.clone();async move {Json(protected)}}))
        .route("/.well-known/oauth-authorization-server",get(move ||{let metadata=metadata.clone();async move {Json(metadata)}}))
        .route("/register",register);
    let server=tokio::spawn(async move {axum::serve(listener,app).with_graceful_shutdown(async {let _=stopped.await;}).await.unwrap();});
    let root=tempfile::tempdir().unwrap();let mut provider=McpOAuthProvider::new(McpTokenStore::new(root.path(),"dcr",&format!("http://{address}")));provider.redirect_url=Some("http://127.0.0.1:0/callback".to_string());provider.scopes=Some(vec!["mcp".into()]);provider.require_https=false;
    let client=reqwest::Client::new();let begin=begin_authorization(&mut provider,&client).await.unwrap();
    let body=tokio::time::timeout(Duration::from_secs(2),registrations.recv()).await.unwrap().unwrap();
    let object=body.as_object().unwrap();
    assert_eq!(object.len(),5,"the pinned ClientRegistrationRequest has exactly five fields");
    for key in ["redirect_uris","grant_types","response_types","token_endpoint_auth_method","client_name"] { assert!(object.contains_key(key),"missing {key}"); }
    assert_eq!(body["grant_types"],json!(["authorization_code","refresh_token"]));
    assert_eq!(body["response_types"],json!(["code"]));
    assert_eq!(body["client_name"],"oh-my-openagent");assert_eq!(body["token_endpoint_auth_method"],"none");assert!(body.get("scope").is_none());
    let authorization=begin.authorization_url.unwrap();
    let resource=authorization.query_pairs().find(|(key,_)|key=="resource").map(|(_,value)|value.into_owned());
    assert_eq!(resource.as_deref(),Some(format!("http://{address}/").as_str()));
    drop(client);stop.send(()).unwrap();tokio::time::timeout(Duration::from_secs(2),server).await.unwrap().unwrap();
}
