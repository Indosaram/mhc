use maho_ext_pi_websearch::websearch::{search::*,types::*};
use tokio::{io::{AsyncReadExt,AsyncWriteExt},net::TcpListener};
use tokio_util::sync::CancellationToken;
#[tokio::test]
async fn real_http_primary_failure_falls_back_and_reports_attempts(){
    let listener=TcpListener::bind("127.0.0.1:0").await.unwrap_or_else(|error|panic!("bind: {error}"));let address=listener.local_addr().unwrap_or_else(|error|panic!("address: {error}"));
    let server=tokio::spawn(async move{
        for (status,body) in [("503 Service Unavailable",r#"{"error":"down"}"#),("200 OK",r#"{"results":[{"title":"Fallback","url":"https://fallback.example.com","text":"result"}]}"#)]{
            let (mut stream,_)=listener.accept().await.unwrap_or_else(|error|panic!("accept: {error}"));let mut bytes=Vec::new();
            loop{let mut buffer=[0;1024];let read=stream.read(&mut buffer).await.unwrap_or_else(|error|panic!("read: {error}"));assert_ne!(read,0);bytes.extend_from_slice(&buffer[..read]);if bytes.windows(4).any(|window|window==b"\r\n\r\n"){break;}}
            let response=format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len());stream.write_all(response.as_bytes()).await.unwrap_or_else(|error|panic!("write: {error}"));
        }
    });
    let providers=["primary","fallback"].into_iter().map(|id|serde_json::from_value(serde_json::json!({"provider":"exa","id":id,"baseUrl":format!("http://{address}/{id}")})).unwrap_or_else(|error|panic!("config: {error}"))).collect();
    let config=WebsearchConfig{strategy:RoutingStrategy::Priority,fallback:true,auto:true,providers};let request=SearchRequest{query:"route test".into(),max_results:3.0,allowed_domains:None,blocked_domains:None};let mut state=create_search_routing_state(2);
    let details=tokio::time::timeout(std::time::Duration::from_secs(5),perform_search(&reqwest::Client::new(),&config,&request,None,&mut state,None)).await.unwrap_or_else(|error|panic!("search timeout: {error}")).unwrap_or_else(|error|panic!("search: {error}"));
    assert_eq!(details.entry_id.as_deref(),Some("fallback"));assert_eq!(details.results.len(),1);let attempts=details.attempts.unwrap_or_else(||panic!("missing attempts"));assert_eq!(attempts.len(),2);assert_eq!(attempts[0].error.as_deref(),Some("Search failed with HTTP 503: down"));assert_eq!(state.success_counts,vec![0.0,1.0]);server.await.unwrap_or_else(|error|panic!("server: {error}"));
}
#[tokio::test]
async fn preaborted_search_propagates_without_network(){
    let signal=CancellationToken::new();signal.cancel();let config:SearchProviderEntry=serde_json::from_value(serde_json::json!({"provider":"exa"})).unwrap_or_else(|error|panic!("config: {error}"));let request=SearchRequest{query:"abort".into(),max_results:1.0,allowed_domains:None,blocked_domains:None};
    assert!(perform_provider_search(&reqwest::Client::new(),&config,&request,Some(&signal)).await.is_err());
}
