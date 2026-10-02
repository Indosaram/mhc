use maho_ext_pi_webfetch::webfetch::fetcher::*;
use tokio::io::{AsyncReadExt,AsyncWriteExt};
#[tokio::test]async fn plain_http_surface(){let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();let address=listener.local_addr().unwrap();let server=tokio::spawn(async move{let(mut socket,_)=listener.accept().await.unwrap();let mut buffer=[0;4096];let n=socket.read(&mut buffer).await.unwrap();let request=String::from_utf8_lossy(&buffer[..n]).to_lowercase();assert!(request.contains("sec-fetch-mode: navigate"));socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 5\r\nConnection: close\r\n\r\nready").await.unwrap();});let r=fetch_url(&format!("http://{address}"),WebfetchFormat::Text,Some(7.0)).await.unwrap();assert_eq!(r.body,b"ready");assert_eq!(r.status,200);server.await.unwrap();}
#[tokio::test]async fn unreachable_url_error(){let listener=std::net::TcpListener::bind("127.0.0.1:0").unwrap();let address=listener.local_addr().unwrap();drop(listener);let r=fetch_url(&format!("http://{address}"),WebfetchFormat::Text,Some(1.0)).await;assert!(r.is_err());}
#[test]fn invalid_scheme(){let r=validate_url("file:///tmp/secret");assert_eq!(r.unwrap_err().to_string(),"URL must start with http:// or https://");}
#[test]fn timeout_bounds(){assert_eq!(clamp_timeout(None),30);assert_eq!(clamp_timeout(Some(f64::NAN)),30);assert_eq!(clamp_timeout(Some(-1.0)),30);assert_eq!(clamp_timeout(Some(1.5)),2);assert_eq!(clamp_timeout(Some(200.0)),120);}
#[tokio::test]
async fn abort_after_headers_cancels_body_read(){
    let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap_or_else(|error|panic!("bind: {error}"));let address=listener.local_addr().unwrap_or_else(|error|panic!("address: {error}"));
    let (headers_sent,headers_received)=tokio::sync::oneshot::channel();
    let server=tokio::spawn(async move{let (mut socket,_)=listener.accept().await.unwrap_or_else(|error|panic!("accept: {error}"));let mut request=Vec::new();
        loop{let mut buffer=[0;4096];let count=socket.read(&mut buffer).await.unwrap_or_else(|error|panic!("read: {error}"));assert_ne!(count,0);request.extend_from_slice(&buffer[..count]);if request.windows(4).any(|window|window==b"\r\n\r\n"){break;}}
        socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n").await.unwrap_or_else(|error|panic!("headers: {error}"));headers_sent.send(()).unwrap_or_else(|_|panic!("header signal receiver dropped"));let mut buffer=[0;1];assert_eq!(socket.read(&mut buffer).await.unwrap_or_else(|error|panic!("disconnect: {error}")),0);});
    let signal=tokio_util::sync::CancellationToken::new();let request_signal=signal.clone();let fetch=tokio::spawn(async move{fetch_url_with_signal(&format!("http://{address}"),WebfetchFormat::Text,Some(5.0),Some(&request_signal)).await});
    tokio::time::timeout(std::time::Duration::from_secs(5),headers_received).await.unwrap_or_else(|error|panic!("header timeout: {error}")).unwrap_or_else(|error|panic!("header signal: {error}"));signal.cancel();
    let result=fetch.await.unwrap_or_else(|error|panic!("fetch task: {error}"));assert!(matches!(result,Err(maho_ext_pi_webfetch::webfetch::errors::WebfetchError::Aborted)));tokio::time::timeout(std::time::Duration::from_secs(5),server).await.unwrap_or_else(|error|panic!("server timeout: {error}")).unwrap_or_else(|error|panic!("server task: {error}"));
}
