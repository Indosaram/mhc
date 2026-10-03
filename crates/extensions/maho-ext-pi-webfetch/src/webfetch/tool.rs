pub const DEFAULT_OUTPUT_MAX_BYTES:usize=50*1024;
use maho_ext_api::{AgentToolResult, ExtensionFailure, ToolDefinition, ToolError, ToolResult};
use serde_json::{Value, json};
use std::sync::Arc;

pub fn definition() -> ToolDefinition {
    let mut tool = ToolDefinition::new("webfetch",
        "Fetches content from a URL and returns it as markdown, plain text, or HTML. Network use is bounded by timeout and response size limits.",
        json!({"type":"object","required":["url"],"properties":{
            "url":{"type":"string","description":"The URL to fetch content from"},
            "format":{"type":"string","enum":["markdown","text","html"],"description":"The format to return the content in. Defaults to markdown."},
            "timeout":{"type":"number","description":"Optional timeout in seconds. Maximum 120."}}}),
        Arc::new(|call| Box::pin(async move {
            let result = tokio::select! { biased;
                () = call.signal.cancelled() => return Err(ToolError::Message("Request aborted".into())),
                result = execute(call.params, None, None) => result.map_err(|error| ToolError::Message(error.message))?,
            };
            let text = result.content.iter().find_map(|block| match block {
                maho_ext_api::ContentBlock::Text(text) => Some(text.text.clone()), _ => None,
            }).unwrap_or_default();
            let mut output = ToolResult::text(text);
            output.details = Some(result.details);
            Ok(output)
        })));
    tool.label = "Web Fetch".into();
    tool.prompt_snippet = Some("webfetch: retrieve URL content as markdown, text, or html".into());
    tool.prompt_guidelines = Some(vec!["Use webfetch when a specific URL must be retrieved.".into(),
        "Prefer markdown format unless raw HTML or plain text is explicitly needed.".into(),
        "The tool is read-only and does not modify files.".into()]);
    tool
}

pub async fn execute(params: Value, signal: Option<maho_ai::utils::abort::AbortSignal>,
    update: Option<maho_agent::types::AgentToolUpdateCallback>) -> Result<AgentToolResult, ExtensionFailure> {
    let url = params["url"].as_str().ok_or_else(|| ExtensionFailure::new("url must be a string"))?;
    let format = parse_webfetch_format(params.get("format"));
    let format_name = match format { super::fetcher::WebfetchFormat::Markdown => "markdown", super::fetcher::WebfetchFormat::Text => "text", super::fetcher::WebfetchFormat::Html => "html" };
    let timeout = super::fetcher::clamp_timeout(params["timeout"].as_f64());
    if let Some(update) = update {
        let mut progress = AgentToolResult::text(format!("Fetching {url} as {format_name} (timeout {timeout}s)"));
        progress.details = json!({"phase":"fetching","url":url,"format":format_name,"timeoutSeconds":timeout});
        update(progress);
    }
    let fetched = super::fetcher::fetch_url_with_abort(url, format, params["timeout"].as_f64(), signal.as_ref()).await
        .map_err(|error| ExtensionFailure::new(error.to_string()))?;
    let raw = String::from_utf8_lossy(&fetched.body);
    let content_type = fetched.content_type.to_lowercase();
    let html = content_type.contains("text/html") || content_type.contains("application/xhtml+xml");
    let converted = html && format != super::fetcher::WebfetchFormat::Html;
    let text = match (html, format) {
        (true, super::fetcher::WebfetchFormat::Markdown) => super::content::html_to_markdown(&raw, &fetched.url),
        (true, super::fetcher::WebfetchFormat::Text) => super::content::html_to_text(&raw, &fetched.url),
        _ => raw.into_owned(),
    };
    let capped = cap_webfetch_output(&text);
    let mut result = AgentToolResult::text(capped.text);
    result.details = json!({"url":url,"finalUrl":fetched.url,"format":format_name,"status":fetched.status,
        "statusText":fetched.status_text,"contentType":fetched.content_type,"bytes":fetched.bytes,"timeoutSeconds":timeout,
        "converted":converted,"truncated":fetched.truncated,"outputTruncated":capped.truncated,
        "outputBytes":capped.output_bytes,"outputTotalBytes":capped.total_bytes});
    Ok(result)
}
pub fn parse_webfetch_format(value:Option<&serde_json::Value>)->super::fetcher::WebfetchFormat{
    match value.and_then(serde_json::Value::as_str){Some("text")=>super::fetcher::WebfetchFormat::Text,Some("html")=>super::fetcher::WebfetchFormat::Html,_=>super::fetcher::WebfetchFormat::Markdown}
}
#[derive(Debug,PartialEq,Eq)]
pub struct WebfetchOutputCap{pub text:String,pub truncated:bool,pub output_bytes:usize,pub total_bytes:usize}
pub fn cap_webfetch_output(text:&str)->WebfetchOutputCap{
    let total_bytes=text.len();
    if total_bytes<=DEFAULT_OUTPUT_MAX_BYTES{return WebfetchOutputCap{text:text.into(),truncated:false,output_bytes:total_bytes,total_bytes};}
    let mut lines=Vec::new();let mut used=0;
    for line in text.split('\n'){
        let bytes=line.len()+usize::from(!lines.is_empty());
        if used+bytes>DEFAULT_OUTPUT_MAX_BYTES{break;}
        lines.push(line);used+=bytes;
    }
    let head=if lines.is_empty(){
        let mut end=DEFAULT_OUTPUT_MAX_BYTES;while !text.is_char_boundary(end){end-=1;}
        let prefix=&text[..end];if end==DEFAULT_OUTPUT_MAX_BYTES{prefix.strip_suffix('\u{fffd}').unwrap_or(prefix).to_owned()}else{prefix.to_owned()}
    }else{lines.join("\n")};
    let output_bytes=head.len();
    let notice=format!("\n\n[Output truncated: {} of {} shown ({} limit). Re-fetch a more specific URL or use web_search for targeted content.]",format_byte_size(output_bytes),format_byte_size(total_bytes),format_byte_size(DEFAULT_OUTPUT_MAX_BYTES));
    WebfetchOutputCap{text:format!("{head}{notice}"),truncated:true,output_bytes,total_bytes}
}
fn format_byte_size(bytes:usize)->String{
    if bytes<1024{return format!("{bytes}B");}
    let number=bytes.to_string().parse::<f64>().unwrap_or_else(|_|unreachable!("usize decimal fits f64"));
    if bytes<1024*1024{format!("{:.1}KB",number/1024.0)}else{format!("{:.1}MB",number/(1024.0*1024.0))}
}
