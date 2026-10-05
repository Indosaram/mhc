#[path = "../tests/support/mod.rs"]
mod support;
use maho_ext_api::ContentBlock;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let tool = support::registered_tool();
    let cases:serde_json::Value=serde_json::from_str(include_str!("../tests/fixtures/pinned-linkedom-verifier-delta.json"))?;
    for case in cases.as_array().ok_or("cases")?{
        for format in ["markdown","text"]{
            let body=case["html"].as_str().ok_or("html")?.to_owned();
            let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await?;let address=listener.local_addr()?;
            let mut server=tokio::spawn(async move{let(mut socket,_)=listener.accept().await?;let mut request=Vec::new();loop{let mut buffer=[0;4096];let count=socket.read(&mut buffer).await?;if count==0{return Err(std::io::ErrorKind::UnexpectedEof.into());}request.extend_from_slice(&buffer[..count]);if request.windows(4).any(|bytes|bytes==b"\r\n\r\n"){break;}}socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await?;Ok::<_,std::io::Error>(())});
            let result=tokio::time::timeout(std::time::Duration::from_secs(5),(tool.execute)("qa-linkedom".into(),json!({"url":format!("http://{address}/"),"format":format}),None,None)).await;
            let joined=tokio::time::timeout(std::time::Duration::from_secs(5),&mut server).await;
            if joined.is_err(){server.abort();let _=server.await;}
            joined???;assert!(std::net::TcpListener::bind(address).is_ok());
            let result=result?;assert!(matches!(&result.content[0],ContentBlock::Text(text) if Some(text.text.as_str())==case[format].as_str()));
            println!("PASS actual-pin-linkedom {} {format}; cleanup=joined task, port {address} released",case["name"]);
        }
    }
    for (name, format, content_type, body, status) in [
        ("markdown", "markdown", "text/html", "<h1>Hello</h1><p>Alpha <strong>Beta</strong></p>", "200 OK"),
        ("text", "text", "text/html", "<h1>Hello</h1><p>Alpha<br>Beta</p>", "200 OK"),
        ("html", "html", "text/html", "<p>raw</p>", "200 OK"),
        ("error", "text", "text/plain", "missing", "404 Not Found"),
        ("bom", "text", "text/plain", "\u{feff}ready", "200 OK"),
        ("oversized", "text", "text/plain", "", "200 OK"),
    ] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let length = if name == "oversized" { 5242881 } else { body.len() };
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await?;
            let mut bytes = [0; 4096];
            if socket.read(&mut bytes).await? == 0 { return Err(std::io::ErrorKind::UnexpectedEof.into()); }
            socket.write_all(format!("HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n{body}").as_bytes()).await?;
            Ok::<_, std::io::Error>(())
        });
        let url = format!("http://{address}/{name}");
        println!("invoke webfetch url={url} format={format}");
        let result = (tool.execute)("qa".into(), json!({"url":url,"format":format}), None, None).await;
        let text = match &result.content[0] { ContentBlock::Text(text) => &text.text, _ => return Err("missing text".into()) };
        match name {
            "markdown" => assert_eq!(text, "## Hello\n\nAlpha **Beta**"),
            "text" => assert_eq!(text, "Hello\n\nAlpha\nBeta"),
            "html" => assert_eq!(text, body),
            "error" => { assert_eq!(text, body); assert_eq!(result.details["status"], 404); },
            "bom" => { assert_eq!(text, "ready"); assert_eq!(result.details["bytes"], 8); assert_eq!(result.details["outputBytes"], 5); },
            "oversized" => { assert_eq!(result.is_error, Some(true)); assert_eq!(text, "Response too large (exceeds 5MB limit)"); },
            _ => unreachable!(),
        }
        println!("PASS {name} details={} text={text:?}", result.details);
        server.await??;
        assert!(std::net::TcpListener::bind(address).is_ok());
        println!("cleanup: joined fixture task; port {address} released");
    }
    let controller = maho_ai::utils::abort::AbortController::new();
    controller.abort(Some(maho_ai::utils::abort::AbortReason::new("Error", "qa cancellation")));
    let result = (tool.execute)("qa-cancel".into(), json!({"url":"http://127.0.0.1:1"}), Some(controller.signal()), None).await;
    assert_eq!(result.is_error, Some(true));
    assert!(matches!(&result.content[0], ContentBlock::Text(text) if text.text == "qa cancellation"));
    println!("PASS cancellation invocation=webfetch(http://127.0.0.1:1, pre-aborted reason=qa cancellation); cleanup=no socket created");

    let result = (tool.execute)("qa-invalid".into(), json!({"url":"file:///fixture"}), None, None).await;
    assert_eq!(result.is_error, Some(true));
    assert!(matches!(&result.content[0], ContentBlock::Text(text) if text.text == "URL must start with http:// or https://"));
    println!("PASS invalid URL; cleanup=no socket created");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await?;
        let mut bytes = [0; 4096];
        assert!(socket.read(&mut bytes).await? > 0);
        socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\nx").await?;
        assert_eq!(socket.read(&mut bytes).await?, 0);
        Ok::<_, std::io::Error>(())
    });
    let result = (tool.execute)("qa-timeout".into(), json!({"url":format!("http://{address}/timeout"),"timeout":1}), None, None).await;
    assert_eq!(result.is_error, Some(true));
    assert!(matches!(&result.content[0], ContentBlock::Text(text) if text.text == "Request aborted"));
    tokio::time::timeout(std::time::Duration::from_secs(5), server).await???;
    assert!(std::net::TcpListener::bind(address).is_ok());
    println!("PASS body timeout; cleanup=joined fixture task, port {address} released");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move {
        for (status, body, location) in [("302 Found", "discard", "Location: /final\r\n"), ("200 OK", "redirected", "")] {
            let (mut socket, _) = listener.accept().await?;
            let mut bytes = [0; 4096];
            if socket.read(&mut bytes).await? == 0 { return Err(std::io::ErrorKind::UnexpectedEof.into()); }
            socket.write_all(format!("HTTP/1.1 {status}\r\n{location}Content-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await?;
        }
        Ok::<_, std::io::Error>(())
    });
    let result = (tool.execute)("qa-redirect".into(), json!({"url":format!("http://{address}/start"),"format":"text"}), None, None).await;
    assert_eq!(result.details["finalUrl"], format!("http://{address}/final"));
    assert!(matches!(&result.content[0], ContentBlock::Text(text) if text.text == "redirected"));
    server.await??;
    assert!(std::net::TcpListener::bind(address).is_ok());
    println!("PASS redirect invocation=webfetch(http://{address}/start,text) finalUrl={}; cleanup=joined task, port released", result.details["finalUrl"]);

    for body in ["\u{1f600}".repeat(20000), "hello world\n".repeat(10000)] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let expected = body.len();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await?;
            let mut bytes = [0; 4096];
            if socket.read(&mut bytes).await? == 0 { return Err(std::io::ErrorKind::UnexpectedEof.into()); }
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await?;
            Ok::<_, std::io::Error>(())
        });
        let result = (tool.execute)("qa-cap".into(), json!({"url":format!("http://{address}/cap"),"format":"text"}), None, None).await;
        assert_eq!(result.details["outputTruncated"], true);
        assert_eq!(result.details["outputTotalBytes"], expected);
        assert!(result.details["outputBytes"].as_u64().ok_or("missing output bytes")? <= 51200);
        server.await??;
        assert!(std::net::TcpListener::bind(address).is_ok());
        println!("PASS output-cap invocation=webfetch(http://{address}/cap,text) details={}; cleanup=joined task, port released", result.details);
    }
    Ok(())
}
