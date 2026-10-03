use super::errors::WebfetchError;
pub const MAX_RESPONSE_SIZE_BYTES:usize=5*1024*1024;
pub const DEFAULT_TIMEOUT_SECONDS:u64=30;
pub const MAX_TIMEOUT_SECONDS:u64=120;
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum WebfetchFormat{Markdown,Text,Html}
pub struct FetchResult{pub url:String,pub status:u16,pub status_text:String,pub content_type:String,pub bytes:usize,pub body:Vec<u8>,pub truncated:bool}
pub fn validate_url(value:&str)->Result<(),WebfetchError>{
    if !value.starts_with("http://")&&!value.starts_with("https://"){return Err(WebfetchError::InvalidUrl("URL must start with http:// or https://".into()));}
    url::Url::parse(value).map_err(|_|WebfetchError::InvalidUrl(format!("Invalid URL: {value}")))?;Ok(())
}
pub fn clamp_timeout(value:Option<f64>)->u64{
    let Some(value)=value.filter(|v|v.is_finite()&&*v>0.0)else{return DEFAULT_TIMEOUT_SECONDS;};
    format!("{:.0}",value.ceil().min(120.0)).parse().unwrap_or(DEFAULT_TIMEOUT_SECONDS)
}
pub const fn build_accept_header(format:WebfetchFormat)->&'static str{match format{
    WebfetchFormat::Markdown=>"text/markdown;q=1.0, text/x-markdown;q=0.9, text/plain;q=0.8, text/html;q=0.7, */*;q=0.1",
    WebfetchFormat::Text=>"text/plain;q=1.0, text/markdown;q=0.9, text/html;q=0.8, */*;q=0.1",
    WebfetchFormat::Html=>"text/html;q=1.0, application/xhtml+xml;q=0.9, text/plain;q=0.8, text/markdown;q=0.7, */*;q=0.1",
}}
pub async fn fetch_url(value:&str,format:WebfetchFormat,timeout:Option<f64>)->Result<FetchResult,WebfetchError>{
    fetch_url_with_signal(value,format,timeout,None).await
}
pub async fn fetch_url_with_signal(value:&str,format:WebfetchFormat,timeout:Option<f64>,signal:Option<&tokio_util::sync::CancellationToken>)->Result<FetchResult,WebfetchError>{
    validate_url(value)?;
    let seconds=clamp_timeout(timeout);
    let reading_body = std::sync::atomic::AtomicBool::new(false);
    let operation=fetch_validated_url(value,format,&reading_body);
    let cancellation=async{match signal{Some(signal)=>signal.cancelled().await,None=>std::future::pending::<()>().await}};
    tokio::select!{biased;()=cancellation=>Err(WebfetchError::Aborted),result=tokio::time::timeout(std::time::Duration::from_secs(seconds),operation)=>result.unwrap_or(Err(WebfetchError::Timeout(seconds)))}
}
pub async fn fetch_url_with_abort(value: &str, format: WebfetchFormat, timeout: Option<f64>, signal: Option<&maho_ai::utils::abort::AbortSignal>) -> Result<FetchResult, WebfetchError> {
    validate_url(value)?;
    let seconds = clamp_timeout(timeout);
    let reading_body = std::sync::atomic::AtomicBool::new(false);
    let operation = fetch_validated_url(value, format, &reading_body);
    let cancelled = async { match signal { Some(signal) => signal.cancelled().await, None => std::future::pending().await } };
    tokio::select! { biased;
        () = cancelled => if reading_body.load(std::sync::atomic::Ordering::Relaxed) { Err(WebfetchError::Aborted) }
            else { Err(WebfetchError::AbortReason(signal.and_then(maho_ai::utils::abort::AbortSignal::reason).map_or_else(|| "Request aborted".into(), |reason| reason.message))) },
        result = tokio::time::timeout(std::time::Duration::from_secs(seconds), operation) => match result {
            Ok(result) => result,
            Err(_) if reading_body.load(std::sync::atomic::Ordering::Relaxed) => Err(WebfetchError::Aborted),
            Err(_) => Err(WebfetchError::Timeout(seconds)),
        },
    }
}
async fn fetch_validated_url(value:&str,format:WebfetchFormat,reading_body:&std::sync::atomic::AtomicBool)->Result<FetchResult,WebfetchError>{
    let client=reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build()?;
    let mut current=value.to_owned();
    for redirects in 0..=20{
        let mut response=client.get(&current).header("Accept",build_accept_header(format)).header("Accept-Language","en-US,en;q=0.9").header("Sec-CH-UA","\"Google Chrome\";v=\"143\", \"Chromium\";v=\"143\", \"Not A(Brand\";v=\"24\"").header("Sec-CH-UA-Mobile","?0").header("Sec-CH-UA-Platform","\"Windows\"").header("Sec-Fetch-Dest","document").header("Sec-Fetch-Mode","navigate").header("Sec-Fetch-Site","none").header("Sec-Fetch-User","?1").header("Upgrade-Insecure-Requests","1").header("User-Agent","Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/143.0.0.0 Safari/537.36").send().await.map_err(|error| network_error(error, &current))?;
        let status=response.status();
        let status_text=response.extensions().get::<hyper::ext::ReasonPhrase>().map_or_else(||status.canonical_reason().unwrap_or("").into(),|reason|reason.as_bytes().iter().copied().map(char::from).collect::<String>());
        let location=response.headers().get_all("location").iter().map(|value|value.as_bytes().iter().copied().map(char::from).collect::<String>()).collect::<Vec<_>>().join(", ");
        if matches!(status.as_u16(),301|302|303|307|308)&&redirects<20&&!location.is_empty(){
            discard_body(response).await;
            current=url::Url::parse(&current).and_then(|url|url.join(&location)).map_err(|_|WebfetchError::InvalidUrl(format!("Invalid URL: {location}")))?.into();continue;
        }
        if response.content_length().is_some_and(|length|length>5*1024*1024){discard_body(response).await;return Err(WebfetchError::ResponseTooLarge);}
        reading_body.store(true, std::sync::atomic::Ordering::Relaxed);
        let content_type=response.headers().get_all("content-type").iter().map(|value|value.as_bytes().iter().copied().map(char::from).collect::<String>()).collect::<Vec<_>>().join(", ");
        let mut body=Vec::new();
        while let Some(chunk)=response.chunk().await.map_err(|error| network_error(error, &current))?{if body.len()+chunk.len()>MAX_RESPONSE_SIZE_BYTES{return Err(WebfetchError::ResponseTooLarge);}body.extend_from_slice(&chunk);}
        let bytes=body.len();return Ok(FetchResult{url:current,status:status.as_u16(),status_text,content_type,bytes,body,truncated:bytes==MAX_RESPONSE_SIZE_BYTES});
    }
    Err(WebfetchError::Aborted)
}
/// Undici's dump-capable path: discard best-effort up to 1024 bytes, then destroy.
/// Dropping the owned response tears down unread body state on every exit path.
pub async fn discard_body(mut response: reqwest::Response) {
    if response.content_length().is_none_or(|length| length <= 1024) {
        let mut discarded = 0;
        while let Ok(Some(chunk)) = response.chunk().await {
            discarded += chunk.len();
            if discarded > 1024 { break; }
        }
    }
}
fn network_error(error: reqwest::Error, url: &str) -> WebfetchError {
    use std::error::Error;
    let mut cause = error.source();
    while let Some(source) = cause {
        if let Some(io) = source.downcast_ref::<std::io::Error>()
            && io.kind() == std::io::ErrorKind::ConnectionRefused
            && let Ok(url) = url::Url::parse(url)
            && let (Some(host), Some(port)) = (url.host_str(), url.port_or_known_default()) {
            return WebfetchError::NetworkMessage { name: "Error", message: format!("connect ECONNREFUSED {host}:{port}"), cause: error };
        }
        if source.to_string().contains("connection closed before message completed") {
            return WebfetchError::NetworkMessage { name: "SocketError", message: "other side closed".into(), cause: error };
        }
        cause = source.source();
    }
    WebfetchError::Network(error)
}
