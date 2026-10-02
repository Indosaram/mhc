use maho_rpc::rpc_client::{ClientFrame, RpcSocketClient};
use serde_json::{Value, json};
use std::{cell::RefCell, time::Duration};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

async fn reply(host: tokio::net::UnixStream, response: Value) -> std::io::Result<()> {
    let mut host = BufReader::new(host);
    let mut request = String::new();
    host.read_line(&mut request).await?;
    let request: Value = serde_json::from_str(&request)?;
    assert_eq!(request["type"], "prompt");
    assert_eq!(request["message"], "hello");
    assert_eq!(request["sessionId"], "lease");
    host.get_mut().write_all(format!("{response}\n{{\"type\":\"message_start\",\"sessionId\":\"lease\"}}\n").as_bytes()).await
}

#[tokio::test]
async fn prompt_hooks_settle_before_the_following_message_start() {
    for data in [json!({"disposition":"started"}), json!({})] {
        let (socket, host) = tokio::net::UnixStream::pair().unwrap();
        let mut client = RpcSocketClient::from_stream(socket);
        client.frames.session_id = Some("lease".into());
        let calls = RefCell::new(Vec::new());
        let response = json!({"type":"response","id":"req_1","success":true,"data":data});
        let serve = reply(host, response);
        let prompt = client.prompt("hello", json!({"sessionTitlePrompt":false}), |_| panic!("event overtook response"),
            |disposition| calls.borrow_mut().push(disposition.into()),
            |success| calls.borrow_mut().push(format!("preflight:{success}")));
        let (result, served) = tokio::time::timeout(Duration::from_secs(2), async { tokio::join!(prompt, serve) }).await.unwrap();
        served.unwrap();
        result.unwrap();
        assert_eq!(*calls.borrow(), [data["disposition"].as_str().unwrap_or("handled"), "preflight:true"]);
        assert!(matches!(client.receive().await.unwrap(), Some(ClientFrame::Event(event)) if event["type"] == "message_start"));
    }
}

#[tokio::test]
async fn rejected_prompt_reports_failed_preflight_once() {
    let (socket, host) = tokio::net::UnixStream::pair().unwrap();
    let mut client = RpcSocketClient::from_stream(socket);
    client.frames.session_id = Some("lease".into());
    let calls = RefCell::new(Vec::new());
    let serve = reply(host, json!({"type":"response","id":"req_1","success":false,"error":"veto"}));
    let prompt = client.prompt("hello", json!({}), |_| {}, |_| panic!("rejected disposition"), |success| calls.borrow_mut().push(success));
    let (result, served) = tokio::time::timeout(Duration::from_secs(2), async { tokio::join!(prompt, serve) }).await.unwrap();
    served.unwrap();
    assert_eq!(result.unwrap_err().to_string(), "veto");
    assert_eq!(*calls.borrow(), [false]);
}

#[tokio::test]
async fn lost_transport_reports_failed_preflight_once() {
    let (socket, host) = tokio::net::UnixStream::pair().unwrap();
    drop(host);
    let mut client = RpcSocketClient::from_stream(socket);
    let calls = RefCell::new(Vec::new());
    assert!(client.prompt("hello", json!({}), |_| {}, |_| panic!("lost disposition"), |success| calls.borrow_mut().push(success)).await.is_err());
    assert_eq!(*calls.borrow(), [false]);
}

#[tokio::test]
async fn collection_completes_on_settled_not_agent_end() {
    let (socket, mut host) = tokio::net::UnixStream::pair().unwrap();
    let mut client = RpcSocketClient::from_stream(socket);
    let collect = client.collect_events(Duration::from_secs(2));
    let serve = async { host.write_all(b"{\"type\":\"agent_end\"}\n{\"type\":\"agent_idle\"}\n{\"type\":\"agent_settled\"}\n").await.unwrap(); };
    let (result, ()) = tokio::join!(collect, serve);
    let events = result.unwrap();
    assert_eq!(events.iter().map(|event|event["type"].as_str().unwrap()).collect::<Vec<_>>(), ["agent_end", "agent_idle", "agent_settled"]);
}

#[tokio::test(start_paused = true)]
async fn collection_has_a_bounded_deadline() {
    let (socket, _host) = tokio::net::UnixStream::pair().unwrap();
    let mut client = RpcSocketClient::from_stream(socket);
    assert_eq!(client.collect_events(Duration::from_secs(60)).await.unwrap_err().kind(), std::io::ErrorKind::TimedOut);
}
