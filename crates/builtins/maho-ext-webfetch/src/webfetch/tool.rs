use super::fetcher::WebfetchFormat;
pub fn requires_html_conversion(content_type:&str,format:WebfetchFormat)->bool { let content_type=content_type.to_lowercase(); format!=WebfetchFormat::Html && (content_type.contains("text/html") || content_type.contains("application/xhtml+xml")) }
pub fn parameters()->serde_json::Value { serde_json::json!({"type":"object","properties":{"url":{"type":"string","description":"The URL to fetch content from"},"format":{"type":"string","enum":["markdown","text","html"],"description":"The format to return the content in. Defaults to markdown."},"timeout":{"type":"number","description":"Optional timeout in seconds. Maximum 120."}},"required":["url"]}) }
pub const DEFAULT_OUTPUT_MAX_BYTES:usize=50*1024;
#[derive(Clone,Debug,PartialEq,Eq)]
pub struct WebfetchOutputCap { pub text:String,pub notice:Option<String>,pub truncated:bool,pub output_bytes:usize,pub total_bytes:usize }
pub fn parse_webfetch_format(value:Option<&str>)->WebfetchFormat { match value { Some("text")=>WebfetchFormat::Text,Some("html")=>WebfetchFormat::Html,_=>WebfetchFormat::Markdown } }
fn format_byte_size(bytes:usize)->String {
    if bytes<1024 { format!("{bytes}B") } else if bytes<1024*1024 { format!("{:.1}KB",bytes as f64/1024.0) } else { format!("{:.1}MB",bytes as f64/(1024.0*1024.0)) }
}
fn take_head_bytes(text:&str,max_bytes:usize)->String {
    let mut lines=vec![]; let mut used=0;
    for line in text.split('\n') { let bytes=line.len()+usize::from(!lines.is_empty()); if used+bytes>max_bytes { break; } lines.push(line); used+=bytes; }
    if !lines.is_empty() { return lines.join("\n"); }
    let mut end=max_bytes.min(text.len()); while !text.is_char_boundary(end) { end-=1; }
    let prefix=&text[..end];
    if end==max_bytes && prefix.ends_with('\u{fffd}') { prefix.strip_suffix('\u{fffd}').unwrap_or(prefix).into() } else { prefix.into() }
}
pub fn cap_webfetch_output(text:&str)->WebfetchOutputCap {
    let total_bytes=text.len();
    if total_bytes<=DEFAULT_OUTPUT_MAX_BYTES { return WebfetchOutputCap{text:text.into(),notice:None,truncated:false,output_bytes:total_bytes,total_bytes}; }
    let head=take_head_bytes(text,DEFAULT_OUTPUT_MAX_BYTES); let output_bytes=head.len();
    let notice=format!("[Output truncated: {} of {} shown ({} limit). Re-fetch a more specific URL or use web_search for targeted content.]",format_byte_size(output_bytes),format_byte_size(total_bytes),format_byte_size(DEFAULT_OUTPUT_MAX_BYTES));
    WebfetchOutputCap{text:head,notice:Some(notice),truncated:true,output_bytes,total_bytes}
}
pub fn create_webfetch_tool()->maho_tools::definition::ToolDefinition {
    use maho_tools::definition::{ToolDefinition,ToolResult,ToolContent,ToolError};
    use serde_json::json;
    use std::sync::{Arc,Mutex};
    let mut tool=ToolDefinition::new("webfetch","Fetches content from a URL and returns it as markdown, plain text, or HTML. Network use is bounded by timeout and response size limits.",parameters(),Arc::new(|call|Box::pin(async move {
        let url=call.params["url"].as_str().ok_or_else(||ToolError::Message("url is required".into()))?;
        let format=parse_webfetch_format(call.params["format"].as_str()); let format_name=match format { WebfetchFormat::Markdown=>"markdown",WebfetchFormat::Text=>"text",WebfetchFormat::Html=>"html" };
        let timeout=super::fetcher::clamp_timeout(call.params["timeout"].as_f64());
        let started=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(|error|ToolError::Message(error.to_string()))?.as_millis();
        let progress=json!({"activity":format!("fetching {url}"),"startedAt":started,"maxWaitMs":timeout*1000});
        let emit=|phase:&str,bytes:Option<usize>,total:Option<usize>| {
            if let Some(update)=&call.on_update {
                let mut details=json!({"phase":phase,"url":url,"format":format_name,"timeoutSeconds":timeout,"progress":progress});
                if let Some(bytes)=bytes { details["bytesRead"]=json!(bytes); }
                if let Some(total)=total { details["totalBytes"]=json!(total); }
                update(ToolResult{content:vec![ToolContent::text(format!("Fetching {url} as {format_name} (timeout {timeout}s)"))],details:Some(details)})?;
            } Ok::<_,ToolError>(())
        };
        emit("fetching",None,None)?;
        let downloaded=Mutex::new(None);
        let on_progress=|bytes,total| {
            *downloaded.lock().unwrap_or_else(std::sync::PoisonError::into_inner)=total;
            emit("downloading",Some(bytes),total).map_err(|error|super::errors::WebfetchError::Abort(error.to_string()))
        };
        let fetched=super::fetcher::fetch_url(super::fetcher::FetchOptions{url,format,timeout_seconds:Some(timeout as f64),signal:Some(&call.signal),on_progress:Some(&on_progress)}).await.map_err(|error|ToolError::Message(error.to_string()))?;
        emit("converting",Some(fetched.bytes),downloaded.into_inner().unwrap_or_else(std::sync::PoisonError::into_inner))?;
        if requires_html_conversion(&fetched.content_type,format) { return Err(ToolError::Message("HTML conversion requires source-equivalent DOM, Readability and Turndown bindings".into())); }
        let raw=String::from_utf8_lossy(&fetched.body); let raw=raw.strip_prefix('\u{feff}').unwrap_or(&raw); let capped=cap_webfetch_output(raw);
        let details=json!({"url":url,"finalUrl":fetched.url,"format":format_name,"status":fetched.status,"statusText":fetched.status_text,"contentType":fetched.content_type,"bytes":fetched.bytes,"timeoutSeconds":timeout,"converted":false,"truncated":fetched.truncated,"outputTruncated":capped.truncated,"outputBytes":capped.output_bytes,"outputTotalBytes":capped.total_bytes});
        let mut content=vec![ToolContent::text(if capped.notice.is_some() { format!("{}\n",capped.text) } else { capped.text })];
        if let Some(notice)=capped.notice { content.push(ToolContent::Text{text:notice,audience:Some("model".into())}); }
        Ok(ToolResult{content,details:Some(details)})
    })));
    tool.label="Web Fetch".into(); tool.prompt_snippet=Some("webfetch: retrieve URL content as markdown, text, or html".into());
    tool.prompt_guidelines=Some(vec!["Use webfetch when a specific URL must be retrieved.".into(),"Prefer markdown format unless raw HTML or plain text is explicitly needed.".into(),"The tool is read-only and does not modify files.".into()]); tool
}
#[cfg(test)] mod tests {
    use super::*;
    #[tokio::test] async fn native_output_caps_match_upstream_single_line_multiline_and_small_cases() {
        use tokio::io::{AsyncReadExt,AsyncWriteExt};
        let bodies=[format!("{{\"data\":\"{}\"}}","x".repeat(2*1024*1024)),(0..5000).map(|index|format!("line-{index}-{}\n","y".repeat(40))).collect::<String>(),"hello world\nsecond line\n".into()];
        for (index,body) in bodies.into_iter().enumerate() {
            let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap(); let url=format!("http://{}/",listener.local_addr().unwrap());
            let server=async {let (mut socket,_)=listener.accept().await.unwrap();let mut request=[0;4096];assert!(socket.read(&mut request).await.unwrap()>0);socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",body.len()).as_bytes()).await.unwrap();socket.write_all(body.as_bytes()).await.unwrap();};
            let client=async {(create_webfetch_tool().execute)(maho_tools::definition::ToolCall{id:"cap",params:serde_json::json!({"url":url,"format":"text"}),signal:Default::default(),context:None,on_update:None}).await.unwrap()};
            let (_,result)=tokio::time::timeout(std::time::Duration::from_secs(5),async {tokio::join!(server,client)}).await.unwrap();
            let details=result.details.unwrap();assert_eq!(details["outputTotalBytes"],body.len());assert_eq!(details["outputTruncated"],index<2);
            let maho_tools::definition::ToolContent::Text{text,..}=&result.content[0] else {panic!("expected fetched text")};
            if index<2 {assert!(details["outputBytes"].as_u64().unwrap()<=DEFAULT_OUTPUT_MAX_BYTES as u64);assert!(text.starts_with(if index==0 {"{\"data\":\"xxxx"} else {"line-0-"}));assert!(matches!(&result.content[1],maho_tools::definition::ToolContent::Text{audience:Some(audience),..} if audience=="model"));}
            else {assert_eq!(text,&body);assert_eq!(result.content.len(),1);}
        }
    }
    #[tokio::test] async fn native_executor_fetches_text_and_emits_progress_metadata() {
        use tokio::io::{AsyncReadExt,AsyncWriteExt};
        let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap(); let url=format!("http://{}/",listener.local_addr().unwrap());
        let server=async { let (mut socket,_)=listener.accept().await.unwrap(); let mut buffer=[0;4096]; assert!(socket.read(&mut buffer).await.unwrap()>0); socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello").await.unwrap(); };
        let updates=std::sync::Arc::new(std::sync::Mutex::new(Vec::new())); let capture=updates.clone();
        let client=async {
            let result=(create_webfetch_tool().execute)(maho_tools::definition::ToolCall{id:"fetch",params:serde_json::json!({"url":url}),signal:Default::default(),context:None,on_update:Some(std::sync::Arc::new(move |update| { capture.lock().unwrap().push(update); Ok(()) }))}).await.unwrap();
            assert_eq!(result.content,[maho_tools::definition::ToolContent::text("hello")]); let details=result.details.unwrap(); assert_eq!(details["status"],200); assert_eq!(details["outputBytes"],5); assert_eq!(details["converted"],false);
        };
        tokio::time::timeout(std::time::Duration::from_secs(5),async { tokio::join!(server,client); }).await.unwrap();
        let updates=updates.lock().unwrap(); assert_eq!(updates.first().unwrap().details.as_ref().unwrap()["phase"],"fetching"); assert!(updates.iter().any(|update|update.details.as_ref().unwrap()["phase"]=="downloading")); assert_eq!(updates.last().unwrap().details.as_ref().unwrap()["phase"],"converting");
    }
    #[test] fn only_html_text_and_markdown_require_conversion() { assert!(requires_html_conversion("Text/HTML; charset=utf-8",WebfetchFormat::Markdown)); assert!(requires_html_conversion("application/xhtml+xml",WebfetchFormat::Text)); assert!(!requires_html_conversion("text/html",WebfetchFormat::Html)); assert!(!requires_html_conversion("application/json",WebfetchFormat::Markdown)); }
    #[test] fn small_output_is_unchanged() { assert_eq!(cap_webfetch_output("hello"),WebfetchOutputCap{text:"hello".into(),notice:None,truncated:false,output_bytes:5,total_bytes:5}); }
    #[test] fn whole_lines_are_kept() { let input=format!("first\n{}", "x".repeat(DEFAULT_OUTPUT_MAX_BYTES)); let result=cap_webfetch_output(&input); assert_eq!(result.text,"first"); assert_eq!(result.output_bytes,5); assert!(result.truncated); }
    #[test] fn oversized_first_line_keeps_utf8_safe_prefix() { let result=cap_webfetch_output(&"한".repeat(DEFAULT_OUTPUT_MAX_BYTES)); assert_eq!(result.output_bytes,DEFAULT_OUTPUT_MAX_BYTES-2); assert!(result.text.chars().all(|c|c=='한')); }
    #[test] fn exact_ceiling_is_not_truncated() { assert!(!cap_webfetch_output(&"x".repeat(DEFAULT_OUTPUT_MAX_BYTES)).truncated); }
    #[test] fn unknown_format_defaults_to_markdown() { assert_eq!(parse_webfetch_format(Some("invalid")),WebfetchFormat::Markdown); assert_eq!(parse_webfetch_format(None),WebfetchFormat::Markdown); }
}
