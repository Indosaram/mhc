use maho_ext_pi_webfetch::webfetch::fetcher::*;
use tokio::io::{AsyncReadExt,AsyncWriteExt};
#[tokio::test]
async fn malformed_transport_errors_preserve_pinned_undici_taxonomy() {
    for (response, name, message) in [
        ("NOTHTTP\r\n\r\n", "HTTPParserError", "Response does not match the HTTP/1.1 protocol (Expected HTTP/, RTSP/ or ICE/)"),
        ("HTTP/1.1 200 OK\r\nBad Header: x\r\nContent-Length: 0\r\n\r\n", "HTTPParserError", "Response does not match the HTTP/1.1 protocol (Invalid header token)"),
        ("HTTP/1.1 200 OK\r\nContent-Length: 8\r\nConnection: close\r\n\r\nabc", "ResponseContentLengthMismatchError", "Response body length does not match content-length header"),
    ] {
        let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("listener");let address=listener.local_addr().expect("address");
        let mut server=tokio::spawn(async move {let(mut socket,_)=listener.accept().await.expect("accept");let mut request=Vec::new();loop{let mut buffer=[0;4096];let count=socket.read(&mut buffer).await.expect("request");assert_ne!(count,0);request.extend_from_slice(&buffer[..count]);if request.windows(4).any(|bytes|bytes==b"\r\n\r\n"){break;}}socket.write_all(response.as_bytes()).await.expect("response");socket.shutdown().await.expect("shutdown");});
        let result=tokio::time::timeout(std::time::Duration::from_secs(5),fetch_url(&format!("http://{address}/"),WebfetchFormat::Text,Some(2.0))).await;
        let joined=tokio::time::timeout(std::time::Duration::from_secs(5),&mut server).await;
        if joined.is_err(){server.abort();let _=server.await;}
        joined.expect("server deadline").expect("server");
        let error=match result.expect("request deadline"){Err(error)=>error,Ok(_)=>panic!("expected transport failure")};
        assert!(std::net::TcpListener::bind(address).is_ok());
        assert_eq!(error.name(),name,"{error:?}");assert_eq!(error.to_string(),message);
    }
}
#[tokio::test]
async fn redirect_limit_returns_twentieth_redirect_response_and_releases_port() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("address");
    let mut server = tokio::spawn(async move {
        let mut paths = Vec::new();
        for index in 0..=20 {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let mut request = Vec::new();
            loop {
                let mut buffer = [0; 4096];
                let count = socket.read(&mut buffer).await.expect("request");
                assert_ne!(count, 0);
                request.extend_from_slice(&buffer[..count]);
                if request.windows(4).any(|bytes| bytes == b"\r\n\r\n") { break; }
            }
            paths.push(String::from_utf8(request).expect("HTTP").split_whitespace().nth(1).expect("path").to_owned());
            let body = if index == 20 { "limit body" } else { "" };
            socket.write_all(format!("HTTP/1.1 302 Custom Redirect\r\nLocation: /{}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", index + 1, body.len()).as_bytes()).await.expect("response");
        }
        paths
    });
    let result = tokio::time::timeout(std::time::Duration::from_secs(10), fetch_url(&format!("http://{address}/0"), WebfetchFormat::Text, Some(5.0))).await;
    let joined = tokio::time::timeout(std::time::Duration::from_secs(5), &mut server).await;
    if joined.is_err() { server.abort(); let _ = server.await; }
    let paths = joined.expect("server deadline").expect("server");
    let result = result.expect("request deadline").expect("fetch");
    assert_eq!(paths, (0..=20).map(|index| format!("/{index}")).collect::<Vec<_>>());
    assert_eq!(result.url, format!("http://{address}/20"));
    assert_eq!(result.status, 302);
    assert_eq!(result.status_text, "Custom Redirect");
    assert_eq!(result.body, b"limit body");
    assert!(std::net::TcpListener::bind(address).is_ok());
}
#[tokio::test]async fn plain_http_surface(){let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();let address=listener.local_addr().unwrap();let server=tokio::spawn(async move{let(mut socket,_)=listener.accept().await.unwrap();let mut bytes=Vec::new();loop{let mut buffer=[0;4096];let count=socket.read(&mut buffer).await.unwrap();assert_ne!(count,0);bytes.extend_from_slice(&buffer[..count]);if bytes.windows(4).any(|window|window==b"\r\n\r\n"){break;}}let request=String::from_utf8_lossy(&bytes).to_lowercase();assert!(request.contains("sec-fetch-mode: navigate"));socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 5\r\nConnection: close\r\n\r\nready").await.unwrap();});let r=fetch_url(&format!("http://{address}"),WebfetchFormat::Text,Some(7.0)).await.unwrap();assert_eq!(r.body,b"ready");assert_eq!(r.status,200);tokio::time::timeout(std::time::Duration::from_secs(5),server).await.expect("server timeout").unwrap();}
#[tokio::test]async fn unreachable_url_error(){let listener=std::net::TcpListener::bind("127.0.0.1:0").unwrap();let address=listener.local_addr().unwrap();drop(listener);let r=fetch_url(&format!("http://{address}"),WebfetchFormat::Text,Some(1.0)).await;assert!(r.is_err());}
#[test]fn invalid_scheme(){let r=validate_url("file:///tmp/secret");assert_eq!(r.unwrap_err().to_string(),"URL must start with http:// or https://");}
#[test]fn timeout_bounds(){assert_eq!(clamp_timeout(None),30);assert_eq!(clamp_timeout(Some(f64::NAN)),30);assert_eq!(clamp_timeout(Some(-1.0)),30);assert_eq!(clamp_timeout(Some(1.5)),2);assert_eq!(clamp_timeout(Some(200.0)),120);}
#[tokio::test]async fn redirect_declared_body_above_discard_limit_does_not_wait(){
    let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");let address=listener.local_addr().expect("address");
    let server=tokio::spawn(async move{let(mut first,_)=listener.accept().await.expect("accept");let mut request=Vec::new();loop{let mut buffer=[0;4096];let count=first.read(&mut buffer).await.expect("request");assert_ne!(count,0);request.extend_from_slice(&buffer[..count]);if request.windows(4).any(|window|window==b"\r\n\r\n"){break;}}first.write_all(b"HTTP/1.1 302 Found\r\nLocation: /next\r\nContent-Length: 1025\r\nConnection: close\r\n\r\n").await.expect("headers");let mut byte=[0;1];assert_eq!(first.read(&mut byte).await.expect("disconnect"),0);
        let(mut second,_)=listener.accept().await.expect("next accept");let mut request=Vec::new();loop{let mut buffer=[0;4096];let count=second.read(&mut buffer).await.expect("next request");assert_ne!(count,0);request.extend_from_slice(&buffer[..count]);if request.windows(4).any(|window|window==b"\r\n\r\n"){break;}}second.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.expect("response");});
    let result=fetch_url(&format!("http://{address}"),WebfetchFormat::Text,Some(1.0)).await;if result.is_err(){server.abort();let _=server.await;}else{tokio::time::timeout(std::time::Duration::from_secs(5),server).await.expect("server timeout").expect("server");}assert_eq!(result.expect("follow without waiting").status,200);
}
#[tokio::test]async fn oversized_header_disposal_disconnects_without_body(){
    let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");let address=listener.local_addr().expect("address");
    let server=tokio::spawn(async move{let(mut socket,_)=listener.accept().await.expect("accept");let mut request=Vec::new();loop{let mut buffer=[0;4096];let count=socket.read(&mut buffer).await.expect("request");assert_ne!(count,0);request.extend_from_slice(&buffer[..count]);if request.windows(4).any(|window|window==b"\r\n\r\n"){break;}}socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5242881\r\nConnection: close\r\n\r\n").await.expect("headers");let mut byte=[0;1];assert_eq!(socket.read(&mut byte).await.expect("disconnect"),0);});
    let result=fetch_url(&format!("http://{address}"),WebfetchFormat::Text,Some(1.0)).await;tokio::time::timeout(std::time::Duration::from_secs(5),server).await.expect("timeout").expect("server");assert!(matches!(result,Err(maho_ext_pi_webfetch::webfetch::errors::WebfetchError::ResponseTooLarge)));
}
#[tokio::test]async fn redirect_waits_for_small_body_discard(){
    let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");let address=listener.local_addr().expect("address");
    let server=tokio::spawn(async move{let(mut first,_)=listener.accept().await.expect("accept");let mut request=Vec::new();loop{let mut buffer=[0;4096];let count=first.read(&mut buffer).await.expect("request");assert_ne!(count,0);request.extend_from_slice(&buffer[..count]);if request.windows(4).any(|window|window==b"\r\n\r\n"){break;}}first.write_all(b"HTTP/1.1 302 Found\r\nLocation: /next\r\nContent-Length: 5\r\nConnection: close\r\n\r\n").await.expect("headers");
        let mut byte=[0;1];tokio::select!{read=first.read(&mut byte)=>{assert_eq!(read.expect("disconnect"),0);},next=listener.accept()=>{let(mut second,_)=next.expect("redirect accept");let mut request=Vec::new();loop{let mut buffer=[0;4096];let count=second.read(&mut buffer).await.expect("next request");assert_ne!(count,0);request.extend_from_slice(&buffer[..count]);if request.windows(4).any(|window|window==b"\r\n\r\n"){break;}}second.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.expect("response");}}});
    let result=fetch_url(&format!("http://{address}"),WebfetchFormat::Text,Some(1.0)).await;tokio::time::timeout(std::time::Duration::from_secs(5),server).await.expect("timeout").expect("server");assert!(matches!(result,Err(maho_ext_pi_webfetch::webfetch::errors::WebfetchError::Timeout(1))));
}
#[tokio::test]async fn custom_status_reason_preserved(){
    let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");let address=listener.local_addr().expect("address");
    let server=tokio::spawn(async move{let(mut socket,_)=listener.accept().await.expect("accept");let mut request=Vec::new();loop{let mut buffer=[0;4096];let count=socket.read(&mut buffer).await.expect("request");assert_ne!(count,0);request.extend_from_slice(&buffer[..count]);if request.windows(4).any(|window|window==b"\r\n\r\n"){break;}}socket.write_all(b"HTTP/1.1 200 Custom\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.expect("response");});
    let result=fetch_url(&format!("http://{address}"),WebfetchFormat::Text,Some(5.0)).await.expect("fetch");tokio::time::timeout(std::time::Duration::from_secs(5),server).await.expect("timeout").expect("server");assert_eq!(result.status_text,"Custom");
}
async fn response_size_boundary(size:usize,declared:bool){
    let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");let address=listener.local_addr().expect("address");
    let server=tokio::spawn(async move{let(mut socket,_)=listener.accept().await.expect("accept");let mut request=Vec::new();loop{let mut buffer=[0;4096];let count=socket.read(&mut buffer).await.expect("request");assert_ne!(count,0);request.extend_from_slice(&buffer[..count]);if request.windows(4).any(|window|window==b"\r\n\r\n"){break;}}let length=if declared{format!("Content-Length: {size}\r\n")}else{String::new()};socket.write_all(format!("HTTP/1.1 200 OK\r\n{length}Connection: close\r\n\r\n").as_bytes()).await.expect("headers");if !declared||size<=MAX_RESPONSE_SIZE_BYTES{socket.write_all(&vec![b'x';size]).await.expect("body");}});
    let result=fetch_url(&format!("http://{address}"),WebfetchFormat::Text,Some(5.0)).await;tokio::time::timeout(std::time::Duration::from_secs(5),server).await.expect("timeout").expect("server");
    if size>MAX_RESPONSE_SIZE_BYTES{assert!(matches!(result,Err(maho_ext_pi_webfetch::webfetch::errors::WebfetchError::ResponseTooLarge)));}else{let result=result.expect("fetch");assert_eq!(result.bytes,size);assert_eq!(result.truncated,size==MAX_RESPONSE_SIZE_BYTES);}
}
#[tokio::test]async fn exact_network_limit_marks_truncated(){response_size_boundary(MAX_RESPONSE_SIZE_BYTES,true).await;}
#[tokio::test]async fn declared_oversize_rejected_before_body(){response_size_boundary(MAX_RESPONSE_SIZE_BYTES+1,true).await;}
#[tokio::test]async fn streamed_oversize_rejected(){response_size_boundary(MAX_RESPONSE_SIZE_BYTES+1,false).await;}
#[tokio::test]async fn repeated_location_headers_join_before_resolution(){
    let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");let address=listener.local_addr().expect("address");
    let server=tokio::spawn(async move{let mut target=String::new();for index in 0..2{let(mut socket,_)=listener.accept().await.expect("accept");let mut request=Vec::new();loop{let mut buffer=[0;4096];let count=socket.read(&mut buffer).await.expect("request");assert_ne!(count,0);request.extend_from_slice(&buffer[..count]);if request.windows(4).any(|window|window==b"\r\n\r\n"){break;}}if index==0{socket.write_all(b"HTTP/1.1 302 Found\r\nLocation: /first\r\nLocation: /second\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.expect("redirect");}else{target=String::from_utf8(request).expect("request text").split_whitespace().nth(1).expect("target").into();socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.expect("response");}}target});
    let result=fetch_url(&format!("http://{address}"),WebfetchFormat::Text,Some(5.0)).await.expect("fetch");let target=tokio::time::timeout(std::time::Duration::from_secs(5),server).await.expect("timeout").expect("server");assert_eq!(target,"/first,%20/second");assert_eq!(result.url,format!("http://{address}/first,%20/second"));
}
#[tokio::test]async fn repeated_content_type_headers_are_joined(){
    let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");let address=listener.local_addr().expect("address");
    let server=tokio::spawn(async move{let(mut socket,_)=listener.accept().await.expect("accept");let mut request=Vec::new();loop{let mut buffer=[0;4096];let count=socket.read(&mut buffer).await.expect("request");assert_ne!(count,0);request.extend_from_slice(&buffer[..count]);if request.windows(4).any(|window|window==b"\r\n\r\n"){break;}}socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Type: charset=utf-8\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.expect("response");});
    let result=fetch_url(&format!("http://{address}"),WebfetchFormat::Text,Some(5.0)).await.expect("fetch");tokio::time::timeout(std::time::Duration::from_secs(5),server).await.expect("timeout").expect("server");assert_eq!(result.content_type,"text/plain, charset=utf-8");
}
#[tokio::test]async fn content_type_preserves_latin1_header_bytes(){
    let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");let address=listener.local_addr().expect("address");
    let server=tokio::spawn(async move{let(mut socket,_)=listener.accept().await.expect("accept");let mut request=Vec::new();loop{let mut buffer=[0;4096];let count=socket.read(&mut buffer).await.expect("request");assert_ne!(count,0);request.extend_from_slice(&buffer[..count]);if request.windows(4).any(|window|window==b"\r\n\r\n"){break;}}socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/plain; label=\xe9\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.expect("response");});
    let result=fetch_url(&format!("http://{address}"),WebfetchFormat::Text,Some(5.0)).await.expect("fetch");tokio::time::timeout(std::time::Duration::from_secs(5),server).await.expect("timeout").expect("server");assert_eq!(result.content_type,"text/plain; label=\u{e9}");
}
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
