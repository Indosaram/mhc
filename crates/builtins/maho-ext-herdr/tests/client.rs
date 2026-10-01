use maho_ext_herdr::client::*;
use std::sync::Arc;
use tokio::{net::UnixListener,io::{AsyncReadExt,AsyncWriteExt}};
#[test]
fn pipe_targets(){assert_eq!(herdr_socket_target("name","win32"),"\\\\.\\pipe\\name");assert_eq!(herdr_socket_target("\\\\?\\pipe\\name","win32"),"\\\\?\\pipe\\name");assert_eq!(herdr_socket_target("/tmp/sock","linux"),"/tmp/sock");}
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
