use serde_json::Value;
use std::collections::HashSet;

fn text<'a>(record: &'a Value, key: &str) -> Option<&'a str> { record.get(key).and_then(Value::as_str) }
fn array<'a>(record: &'a Value, key: &str) -> &'a [Value] { record.get(key).and_then(Value::as_array).map_or(&[],Vec::as_slice) }
fn nonempty(value: Option<&str>) -> Option<&str> { value.filter(|s| !s.is_empty()) }
fn shorten(value: &str, max: usize) -> String {
    if value.encode_utf16().count() <= max { return value.into(); }
    let units = value.encode_utf16().take(max.saturating_sub(1)).collect::<Vec<_>>();
    format!("{}…",String::from_utf16_lossy(&units))
}
fn pluralize(count: usize, singular: &str) -> String { format!("{count} {singular}{}",if count == 1 { "" } else { "s" }) }
fn unique(values: Vec<&str>) -> Vec<&str> { let mut seen = HashSet::new(); values.into_iter().filter(|v| seen.insert(*v)).collect() }
#[derive(Default)]
struct SearchSource<'a> { title: Option<&'a str>, url: Option<&'a str>, snippet: Option<&'a str>, status: Option<&'a str> }
fn source(record: &Value) -> Option<SearchSource<'_>> {
    if !record.is_object() { return None; }
    let result = SearchSource {
        title: nonempty(text(record,"title").or_else(|| text(record,"name"))),
        url: nonempty(text(record,"url").or_else(|| text(record,"uri")).or_else(|| text(record,"retrievedUrl")).or_else(|| text(record,"sourceUrl"))),
        snippet: nonempty(text(record,"snippet").or_else(|| text(record,"text")).or_else(|| text(record,"summary")).or_else(|| text(record,"page_age"))),
        status: nonempty(text(record,"status").or_else(|| text(record,"urlRetrievalStatus"))),
    };
    (result.title.is_some() || result.url.is_some() || result.snippet.is_some() || result.status.is_some()).then_some(result)
}
fn queries(values: Vec<&str>) -> Vec<String> {
    let values = unique(values);
    match values.as_slice() {
        [] => Vec::new(),
        [only] => vec![format!("query: {}",Value::String((*only).into()))],
        _ => std::iter::once("queries:".into()).chain(values.iter().map(|s| format!("- {}",Value::String((*s).into())))).collect(),
    }
}
fn sources(values: &[SearchSource<'_>], label: &str, expanded: bool) -> Vec<String> {
    if values.is_empty() { return Vec::new(); }
    let visible = if expanded { values.len() } else { values.len().min(3) };
    let mut lines = vec![pluralize(values.len(),label)];
    for source in values.iter().take(visible) {
        let title = source.title.map(|v| shorten(v,100));
        let url = source.url.map(|v| shorten(v,120));
        let status = source.status.map_or(String::new(),|v| format!(" ({v})"));
        match (title,url) {
            (Some(title),Some(url)) => lines.push(format!("{title} {url}{status}")),
            (Some(title),None) => lines.push(format!("{title}{status}")),
            (None,Some(url)) => lines.push(format!("{url}{status}")),
            (None,None) => { if let Some(status) = source.status { lines.push(status.into()); } }
        }
        if let Some(snippet) = source.snippet { lines.push(format!("  {}",shorten(snippet,160))); }
    }
    if visible < values.len() { lines.push(format!("… {} more {label}s",values.len()-visible)); }
    lines
}
fn joined(lines: Vec<String>) -> Option<String> { (!lines.is_empty()).then(|| lines.join("\n")) }
fn specialized_body(subtype: &str, raw: &Value, expanded: bool) -> Option<String> {
    match subtype {
        "image_generation_call" => {
            if !raw.is_object() { return None; }
            let mut lines = Vec::new();
            if let Some(status) = nonempty(text(raw,"status")) { lines.push(format!("status: {status}")); }
            if let Some(result) = nonempty(text(raw,"result")) {
                let padding = if result.ends_with("==") { 2 } else if result.ends_with('=') { 1 } else { 0 };
                let bytes = (result.encode_utf16().count()/4).saturating_mul(3).saturating_sub(padding);
                lines.push(format!("{bytes} bytes"));
            }
            if let Some(prompt) = text(raw,"revised_prompt").filter(|s| !s.trim().is_empty()) { lines.push(format!("revised prompt: {}",shorten(prompt,200))); }
            joined(lines)
        }
        "server_tool_use" => {
            nonempty(text(&raw["input"],"query")).map(|q| format!("query: {}",Value::String(q.into())))
                .or_else(|| nonempty(text(raw,"name")).map(|n| format!("name: {n}")))
        }
        "tool_search_tool_result" => {
            let content = raw.get("content").filter(|v| v.is_object())?;
            if let Some(code) = nonempty(text(content,"error_code")) { return Some(match nonempty(text(content,"error_message")) { Some(message) => format!("{code}: {}",shorten(message,200)),None => code.into() }); }
            let names = unique(array(content,"tool_references").iter().filter_map(|r| nonempty(text(r,"tool_name"))).collect());
            if names.is_empty() { return Some("0 tools".into()); }
            let visible = if expanded { names.len() } else { names.len().min(10) };
            let mut lines = vec![pluralize(names.len(),"tool")];
            lines.extend(names.iter().take(visible).map(|v| shorten(v,100)));
            if visible < names.len() { lines.push(format!("… {} more tools",names.len()-visible)); }
            joined(lines)
        }
        "web_search_tool_result" => {
            let results = raw.get("content").and_then(Value::as_array)?;
            let results = results.iter().filter(|r| r.is_object() && text(r,"type") == Some("web_search_result")).collect::<Vec<_>>();
            if results.is_empty() { return Some("0 results".into()); }
            let found = results.iter().filter_map(|r| source(r)).collect::<Vec<_>>();
            Some(sources(&found,"result",expanded).join("\n"))
        }
        "web_search_call" => {
            if !raw.is_object() { return None; }
            let action = &raw["action"];
            let mut lines = Vec::new();
            if let Some(status) = nonempty(text(raw,"status")) { lines.push(format!("status: {status}")); }
            let mut search = Vec::new();
            if let Some(query) = nonempty(text(action,"query")) { search.push(query); }
            search.extend(array(action,"queries").iter().filter_map(Value::as_str));
            lines.extend(queries(search));
            lines.extend(sources(&array(action,"sources").iter().filter_map(source).collect::<Vec<_>>(),"source",expanded));
            joined(lines)
        }
        "groundingMetadata" => {
            if !raw.is_object() { return None; }
            let mut lines = queries(array(raw,"webSearchQueries").iter().filter_map(Value::as_str).collect());
            let found = array(raw,"groundingChunks").iter().filter(|v| v.is_object()).filter_map(|v| source(v.get("web").filter(|web| web.is_object()).unwrap_or(v))).collect::<Vec<_>>();
            lines.extend(sources(&found,"source",expanded));
            joined(lines)
        }
        "urlContextMetadata" => {
            if !raw.is_object() { return None; }
            joined(sources(&array(raw,"urlMetadata").iter().filter_map(source).collect::<Vec<_>>(),"url",expanded))
        }
        _ => None,
    }
}
pub fn stringify_provider_native(raw: &Value) -> Result<String,serde_json::Error> { serde_json::to_string_pretty(raw) }
pub fn format_provider_native_summary(provider: &str, subtype: &str, raw: &Value, expanded: bool) -> String {
    let provider = if provider.is_empty() { String::new() } else { format!("{provider} · ") };
    let marker = if expanded { "▾" } else { "▸" };
    let summary = match subtype {
        "server_tool_use" => nonempty(text(raw,"name")).map(|name| format!("{name} · server_tool_use")),
        "web_search_tool_result" => Some("web_search results".into()),
        "tool_search_tool_result" => Some("tool_search results".into()),
        "web_search_call" => Some(format!("web_search · {}",nonempty(text(raw,"status")).unwrap_or("web_search_call"))),
        "image_generation_call" => Some(format!("image_generation · {}",nonempty(text(raw,"status")).unwrap_or("image_generation_call"))),
        "groundingMetadata" if specialized_body(subtype,raw,expanded).is_some_and(|v| !v.is_empty()) => Some("google_search results".into()),
        "urlContextMetadata" if specialized_body(subtype,raw,expanded).is_some_and(|v| !v.is_empty()) => Some("url_context results".into()),
        _ => None,
    };
    format!("{marker} {provider}{}",summary.unwrap_or_else(|| format!("providerNative · {subtype}")))
}
pub fn format_provider_native_body(subtype: &str, raw: &Value, expanded: bool) -> Result<String,serde_json::Error> {
    if let Some(body) = specialized_body(subtype,raw,expanded).filter(|v| !v.is_empty()) { return Ok(body); }
    let json = stringify_provider_native(raw)?;
    if expanded || json.encode_utf16().count() <= 2000 { return Ok(json); }
    let units = json.encode_utf16().take(2000).collect::<Vec<_>>();
    Ok(format!("{}…",String::from_utf16_lossy(&units)))
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test] fn image_generation_does_not_expose_payload() { let raw = json!({"status":"completed","result":"YWJj","revised_prompt":"cat"}); assert_eq!(format_provider_native_body("image_generation_call",&raw,false).unwrap(),"status: completed\n3 bytes\nrevised prompt: cat"); }
    #[test] fn tool_search_deduplicates_and_limits_results() { let raw = json!({"content":{"tool_references":[{"tool_name":"bash"},{"tool_name":"bash"},{"tool_name":"read"}]}}); assert_eq!(format_provider_native_body("tool_search_tool_result",&raw,false).unwrap(),"2 tools\nbash\nread"); }
    #[test] fn tool_search_error_remains_visible() { let raw = json!({"content":{"error_code":"failed","error_message":"try again"}}); assert_eq!(format_provider_native_body("tool_search_tool_result",&raw,false).unwrap(),"failed: try again"); }
    #[test] fn web_search_queries_are_deduplicated() { let raw = json!({"status":"completed","action":{"query":"a","queries":["a","b"],"sources":[]}}); assert_eq!(format_provider_native_body("web_search_call",&raw,false).unwrap(),"status: completed\nqueries:\n- \"a\"\n- \"b\""); }
    #[test] fn grounding_sources_use_nested_web_record() { let raw = json!({"webSearchQueries":["rust"],"groundingChunks":[{"web":{"title":"Rust","uri":"https://example.test"}}]}); assert_eq!(format_provider_native_body("groundingMetadata",&raw,false).unwrap(),"query: \"rust\"\n1 source\nRust https://example.test"); }
    #[test] fn unknown_subtype_uses_raw_json_and_generic_summary() { assert_eq!(format_provider_native_summary("faux","opaque",&json!({}),false),"▸ faux · providerNative · opaque"); assert_eq!(format_provider_native_body("opaque",&json!({"a":1}),false).unwrap(),"{\n  \"a\": 1\n}"); }
    #[test] fn collapsed_sources_limit_to_three() { let raw = json!({"urlMetadata":[{"url":"a"},{"url":"b"},{"url":"c"},{"url":"d"}]}); assert_eq!(format_provider_native_body("urlContextMetadata",&raw,false).unwrap(),"4 urls\na\nb\nc\n… 1 more urls"); }
    #[test] fn empty_anthropic_search_reports_zero() { assert_eq!(format_provider_native_body("web_search_tool_result",&json!({"content":[]}),false).unwrap(),"0 results"); }
}
