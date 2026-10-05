use futures::StreamExt;
use maho_tools::definition::AbortSignal;
use super::errors::WebfetchError;
pub const MAX_RESPONSE_SIZE_BYTES:usize=5*1024*1024;
pub const DEFAULT_TIMEOUT_SECONDS:u64=30;
pub const MAX_TIMEOUT_SECONDS:u64=120;
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum WebfetchFormat { Markdown,Text,Html }
pub type FetchProgressCallback<'a>=dyn Fn(usize,Option<usize>)->Result<(),WebfetchError>+Send+Sync+'a;
pub struct FetchOptions<'a> {
    pub url:&'a str,pub format:WebfetchFormat,pub timeout_seconds:Option<f64>,pub signal:Option<&'a AbortSignal>,
    pub on_progress:Option<&'a FetchProgressCallback<'a>>,
}
#[derive(Clone,Debug,PartialEq,Eq)]
pub struct FetchResult { pub url:String,pub status:u16,pub status_text:String,pub content_type:String,pub bytes:usize,pub body:Vec<u8>,pub truncated:bool }
pub fn validate_url(url:&str)->Result<(),WebfetchError> {
    if !url.starts_with("http://") && !url.starts_with("https://") { return Err(WebfetchError::InvalidUrl("URL must start with http:// or https://".into())); }
    url::Url::parse(url).map(|_|()).map_err(|_|WebfetchError::InvalidUrl(format!("Invalid URL: {url}")))
}
pub fn clamp_timeout(seconds:Option<f64>)->u64 {
    match seconds { Some(value) if value.is_finite() && value>0.0 => value.ceil().min(MAX_TIMEOUT_SECONDS as f64) as u64,_=>DEFAULT_TIMEOUT_SECONDS }
}
pub const fn build_accept_header(format:WebfetchFormat)-> &'static str {
    match format {
        WebfetchFormat::Markdown=>"text/markdown;q=1.0, text/x-markdown;q=0.9, text/plain;q=0.8, text/html;q=0.7, */*;q=0.1",
        WebfetchFormat::Text=>"text/plain;q=1.0, text/markdown;q=0.9, text/html;q=0.8, */*;q=0.1",
        WebfetchFormat::Html=>"text/html;q=1.0, application/xhtml+xml;q=0.9, text/plain;q=0.8, text/markdown;q=0.7, */*;q=0.1",
    }
}
fn transport_error(error:reqwest::Error)->WebfetchError { WebfetchError::Transport(error.to_string()) }
pub fn parse_content_length(value:&str)->Option<usize> {
    let value=value.trim_start_matches(|character:char|matches!(character,'\u{0009}'..='\u{000d}'|'\u{0020}'|'\u{00a0}'|'\u{1680}'|'\u{2000}'..='\u{200a}'|'\u{2028}'|'\u{2029}'|'\u{202f}'|'\u{205f}'|'\u{3000}'|'\u{feff}'));
    let (negative,digits)=if let Some(rest)=value.strip_prefix('-') { (true,rest) } else { (false,value.strip_prefix('+').unwrap_or(value)) };
    let count=digits.bytes().take_while(u8::is_ascii_digit).count(); if count==0 { return None; }
    let parsed=digits[..count].parse::<f64>().ok()?; if !parsed.is_finite() || (negative && parsed!=0.) { return None; } Some(parsed as usize)
}
pub async fn fetch_url(options:FetchOptions<'_>)->Result<FetchResult,WebfetchError> {
    validate_url(options.url)?;
    let seconds=clamp_timeout(options.timeout_seconds);
    let signal=options.signal.cloned().unwrap_or_default();
    let future=async {
        let client=reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().map_err(transport_error)?;
        let mut current=options.url.to_owned();
        for redirect in 0..=20 {
            let response=client.get(&current)
                .header("Accept",build_accept_header(options.format))
                .header("Accept-Language","en-US,en;q=0.9")
                .header("Sec-CH-UA","\"Google Chrome\";v=\"143\", \"Chromium\";v=\"143\", \"Not A(Brand\";v=\"24\"")
                .header("Sec-CH-UA-Mobile","?0").header("Sec-CH-UA-Platform","\"Windows\"")
                .header("Sec-Fetch-Dest","document").header("Sec-Fetch-Mode","navigate")
                .header("Sec-Fetch-Site","none").header("Sec-Fetch-User","?1")
                .header("Upgrade-Insecure-Requests","1")
                .header("User-Agent","Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/143.0.0.0 Safari/537.36")
                .send().await.map_err(transport_error)?;
            let status=response.status();
            let status_text=response.extensions().get::<hyper::ext::ReasonPhrase>().map_or_else(||status.canonical_reason().unwrap_or("").to_owned(),|reason|reason.as_bytes().iter().map(|byte|char::from(*byte)).collect());
            let location=response.headers().get("location").and_then(|v|v.to_str().ok()).filter(|s|!s.is_empty());
            if matches!(status.as_u16(),301|302|303|307|308) && redirect<20 && let Some(location)=location {
                let next=url::Url::parse(&current).and_then(|base|base.join(location)).map_err(|e|WebfetchError::InvalidUrl(e.to_string()))?;
                super::response_body::discard_body(response.bytes_stream().map(|chunk|chunk.map(|bytes|bytes.to_vec())),MAX_RESPONSE_SIZE_BYTES,Some(&signal)).await;
                current=next.into(); continue;
            }
            let content_type=response.headers().get("content-type").and_then(|v|v.to_str().ok()).unwrap_or("").to_owned();
            let length=response.headers().get("content-length").and_then(|v|v.to_str().ok()).and_then(parse_content_length);
            if length.is_some_and(|n|n>MAX_RESPONSE_SIZE_BYTES) {
                super::response_body::discard_body(response.bytes_stream().map(|chunk|chunk.map(|bytes|bytes.to_vec())),MAX_RESPONSE_SIZE_BYTES,Some(&signal)).await;
                return Err(WebfetchError::ResponseTooLarge("Response too large (exceeds 5MB limit)".into()));
            }
            let mut stream=response.bytes_stream(); let mut body=vec![];
            while let Some(chunk)=stream.next().await {
                let chunk=chunk.map_err(transport_error)?;
                if chunk.len()>MAX_RESPONSE_SIZE_BYTES-body.len() { return Err(WebfetchError::ResponseTooLarge("Response too large (exceeds 5MB limit)".into())); }
                body.extend_from_slice(&chunk);
                if let Some(progress)=options.on_progress { progress(body.len(),length)?; }
            }
            return Ok(FetchResult{url:current,status:status.as_u16(),status_text,content_type,bytes:body.len(),truncated:body.len()==MAX_RESPONSE_SIZE_BYTES,body});
        }
        Err(WebfetchError::Abort("Redirect resolution aborted".into()))
    };
    tokio::select! {
        biased;
        ()=signal.cancelled()=>Err(WebfetchError::Abort("Request aborted".into())),
        result=tokio::time::timeout(std::time::Duration::from_secs(seconds),future)=> match result { Ok(result)=>result,Err(_)=>Err(WebfetchError::Timeout(format!("Request timed out after {seconds}s"))) },
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn content_length_uses_decimal_prefix_and_accepts_negative_zero() { assert_eq!(parse_content_length("\u{feff}+12bytes"),Some(12)); assert_eq!(parse_content_length("1e6"),Some(1)); assert_eq!(parse_content_length("0x10"),Some(0)); assert_eq!(parse_content_length("-0"),Some(0)); assert_eq!(parse_content_length("-1"),None); assert_eq!(parse_content_length("garbage"),None); }
    #[test] fn first_error_path_rejects_non_http_url() { assert!(matches!(validate_url("file:///tmp/file"),Err(WebfetchError::InvalidUrl(_)))); }
    #[test] fn malformed_http_url_is_rejected() { assert!(matches!(validate_url("http://"),Err(WebfetchError::InvalidUrl(_)))); }
    #[test] fn timeout_clamps_rounds_and_defaults() { for value in [None,Some(0.0),Some(-1.0),Some(f64::NAN),Some(f64::INFINITY)] { assert_eq!(clamp_timeout(value),30); } assert_eq!(clamp_timeout(Some(1.1)),2); assert_eq!(clamp_timeout(Some(200.0)),120); }
    #[tokio::test] async fn already_aborted_request_does_not_connect() { let signal=AbortSignal::default(); signal.abort(); let result=fetch_url(FetchOptions{url:"http://127.0.0.1:1/",format:WebfetchFormat::Text,timeout_seconds:None,signal:Some(&signal),on_progress:None}).await; assert!(matches!(result,Err(WebfetchError::Abort(_)))); }
    #[tokio::test]
    async fn real_http_surface_follows_redirect_and_reports_body() {
        use tokio::io::{AsyncReadExt,AsyncWriteExt};
        let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address=listener.local_addr().unwrap();
        let server=async {
            for response in ["HTTP/1.1 302 Found\r\nLocation: /final\r\nContent-Length: 4\r\nConnection: close\r\n\r\nx","HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello"] {
                let (mut socket,_)=listener.accept().await.unwrap();
                let mut buffer=[0u8;4096]; let mut request=vec![];
                loop { let n=socket.read(&mut buffer).await.unwrap(); assert!(n>0); request.extend_from_slice(&buffer[..n]); if request.windows(4).any(|w|w==b"\r\n\r\n") { break; } }
                socket.write_all(response.as_bytes()).await.unwrap();
            }
        };
        let url=format!("http://{address}/start");
        let client=fetch_url(FetchOptions{url:&url,format:WebfetchFormat::Text,timeout_seconds:Some(2.0),signal:None,on_progress:None});
        let (result,())=tokio::join!(client,server);
        let result=result.unwrap(); assert_eq!(result.body,b"hello"); assert_eq!(result.status,200); assert_eq!(result.url,format!("http://{address}/final")); assert_eq!(result.content_type,"text/plain"); assert!(!result.truncated);
    }
}
