use maho_ext_herdr::client::*;
use std::sync::Arc;
use tokio::{net::UnixListener,io::{AsyncReadExt,AsyncWriteExt}};
#[test]
fn pipe_targets(){assert_eq!(herdr_socket_target("name","win32"),"\\\\.\\pipe\\name");assert_eq!(herdr_socket_target("\\\\?\\pipe\\name","win32"),"\\\\?\\pipe\\name");assert_eq!(herdr_socket_target("/tmp/sock","linux"),"/tmp/sock");}
#[tokio::test]
async fn ordered_release_and_drain(){
    let directory=tempfile::tempdir().expect("dir");let path=directory.path().join("sock");let listener=UnixListener::bind(&path).expect("listener");
    let server=tokio::spawn(async move{let mut methods=Vec::new();for _ in 0..2{let (mut socket,_)=listener.accept().await.expect("accept");let mut bytes=Vec::new();loop{let byte=socket.read_u8().await.expect("byte");if byte==b'\n'{break;}bytes.push(byte);}let request:serde_json::Value=serde_json::from_slice(&bytes).expect("request");methods.push(request["method"].as_str().expect("method").to_owned());let response=format!("{{\"id\":{},\"result\":{{}}}}\n",request["id"]);socket.write_all(&response.as_bytes()[..4]).await.expect("split");socket.write_all(&response.as_bytes()[4..]).await.expect("split");}methods});
    let client=HerdrClient::new(path.to_string_lossy().into_owned(),"pane".into(),Arc::new(||500));
    let first=client.send(HerdrMethod::ReportAgent,Default::default());let release=client.send(HerdrMethod::ReleaseAgent,Default::default());let drain=client.drain();
    let (release,first,())=tokio::join!(release,first,drain);release.expect("release");first.expect("first");assert_eq!(server.await.expect("server"),vec!["pane.report_agent","pane.release_agent"]);
}
#[tokio::test]
async fn ndjson_acknowledgement(){
    let directory=tempfile::tempdir().expect("dir");let path=directory.path().join("sock");let listener=UnixListener::bind(&path).expect("listener");
    let server=tokio::spawn(async move{let (mut socket,_)=listener.accept().await.expect("accept");let mut bytes=Vec::new();loop{let byte=socket.read_u8().await.expect("byte");if byte==b'\n'{break;}bytes.push(byte);}let request:serde_json::Value=serde_json::from_slice(&bytes).expect("request");let mut reply=serde_json::to_vec(&serde_json::json!({"id":request["id"],"result":{}})).expect("reply");reply.push(b'\n');socket.write_all(&reply).await.expect("write");request});
    let client=HerdrClient::new(path.to_string_lossy().into_owned(),"pane".into(),Arc::new(||1000));client.send(HerdrMethod::ReportAgent,Default::default()).await.expect("send");let request=server.await.expect("server");assert_eq!(request["method"],"pane.report_agent");assert_eq!(request["params"]["pane_id"],"pane");assert_eq!(request["params"]["source"],"custom:senpi");assert!(request["params"]["seq"].as_u64().expect("seq")>=1_000_000);
}
#[tokio::test]
async fn retries_invalid_replies_with_same_request(){
    for failure in ["malformed","wrong-id","rejected","oversized","end"]{
        let directory=tempfile::tempdir().expect("dir");let path=directory.path().join("sock");let listener=UnixListener::bind(&path).expect("listener");
        let server=tokio::spawn(async move{
            let mut requests=Vec::new();
            for attempt in 0..2{
                let (mut socket,_)=listener.accept().await.expect("accept");let mut bytes=Vec::new();loop{let byte=socket.read_u8().await.expect("byte");if byte==b'\n'{break;}bytes.push(byte);}
                let request:serde_json::Value=serde_json::from_slice(&bytes).expect("request");
                if attempt==0{
                    let reply=match failure{"malformed"=>"{\n".into(),"wrong-id"=>"{\"id\":\"other\",\"result\":{}}\n".into(),"rejected"=>format!("{{\"id\":{},\"error\":{{}}}}\n",request["id"]),"oversized"=>"x".repeat(65_537),_=>String::new()};
                    let _written=socket.write_all(reply.as_bytes()).await;
                }else{let reply=format!("{{\"id\":{},\"result\":{{}}}}\n",request["id"]);socket.write_all(reply.as_bytes()).await.expect("reply");}
                requests.push(request);
            }
            requests
        });
        let client=HerdrClient::new(path.to_string_lossy().into_owned(),"pane".into(),Arc::new(||100));client.send(HerdrMethod::ReportAgent,Default::default()).await.expect("retry");let requests=server.await.expect("server");assert_eq!(requests[0],requests[1],"{failure}");
    }
}

#[tokio::test(start_paused = true)]
async fn timeout_retries_preserve_request_and_drain_after_failure() {
    let directory=tempfile::tempdir().expect("dir");
    let path=directory.path().join("timeout.sock");
    let listener=UnixListener::bind(&path).expect("listener");
    let (seen,mut received)=tokio::sync::mpsc::unbounded_channel();
    let (stop,mut stopped)=tokio::sync::oneshot::channel::<()>();
    let server=tokio::spawn(async move {
        let mut sockets=Vec::new();
        loop {
            tokio::select! {
                () = async { let _closed=(&mut stopped).await; } => break,
                connected=listener.accept() => {
                    let (mut socket,_)=connected.expect("accept");
                    let mut bytes=Vec::new();
                    loop {let byte=socket.read_u8().await.expect("byte");if byte==b'\n'{break;}bytes.push(byte);}
                    seen.send(serde_json::from_slice::<serde_json::Value>(&bytes).expect("request")).expect("request observed");
                    sockets.push(socket);
                }
            }
        }
    });
    let client=HerdrClient::new(path.to_string_lossy().into_owned(),"pane".into(),Arc::new(||500));
    let send=tokio::spawn(client.send(HerdrMethod::ReportAgent,Default::default()));
    let first=received.recv().await.expect("first attempt");
    tokio::time::advance(std::time::Duration::from_millis(500)).await;
    let second=received.recv().await.expect("second attempt");
    assert_eq!(first,second);
    tokio::time::advance(std::time::Duration::from_millis(1500)).await;
    assert!(send.await.expect("sender task").is_err());
    client.drain().await;
    stop.send(()).expect("stop server");
    server.await.expect("server teardown");
    drop(client);
    directory.close().expect("socket directory cleanup");
}
