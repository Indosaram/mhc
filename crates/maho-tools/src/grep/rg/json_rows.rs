use std::collections::BTreeMap;
use base64::Engine;
use serde_json::Value;
use crate::grep::engine::*;
fn decode(value: &Value) -> Result<String,GrepEngineError> {
    if let Some(text) = value["text"].as_str() { return Ok(text.into()); }
    if let Some(bytes) = value["bytes"].as_str() { return base64::engine::general_purpose::STANDARD.decode(bytes).map(|b| String::from_utf8_lossy(&b).into_owned()).map_err(|e| GrepEngineError::EngineUnavailable(e.to_string())); }
    Err(GrepEngineError::EngineUnavailable("Invalid ripgrep JSON text/bytes field".into()))
}
pub fn collect_rows(output: &[u8], display: &str, request: &GrepEngineRequest) -> Result<(Vec<GrepEngineMatch>,bool),GrepEngineError> {
    collect_file_rows(output, display, request, None)
}
pub fn collect_file_rows(output: &[u8], display: &str, request: &GrepEngineRequest, absolute: Option<&std::path::Path>) -> Result<(Vec<GrepEngineMatch>,bool),GrepEngineError> {
    let mut rows = BTreeMap::new(); let mut binary = false;
    for line in String::from_utf8_lossy(output).lines() {
        let event: Value = serde_json::from_str(line).map_err(|e| GrepEngineError::EngineUnavailable(format!("Malformed ripgrep JSON: {e}")))?;
        let kind = event["type"].as_str().ok_or_else(|| GrepEngineError::EngineUnavailable("Invalid ripgrep JSON event".into()))?;
        let data = &event["data"];
        if let Some(absolute) = absolute && kind != "summary" {
            let path = decode(&data["path"])?;
            if std::path::Path::new(&request.cwd).join(path) != absolute { continue; }
        }
        match kind {
            "begin" | "summary" => {},
            "end" => { if !data["binary_offset"].is_null() { binary = true; rows.clear(); } },
            "match" | "context" => {
                let first = data["line_number"].as_u64().ok_or_else(|| GrepEngineError::EngineUnavailable("ripgrep match/context without a begin or line number".into()))?;
                let text = decode(&data["lines"])?; let text = text.strip_suffix('\n').unwrap_or(&text);
                for (index,physical) in text.split('\n').enumerate() {
                    let number = u32::try_from(first.saturating_add(index as u64)).unwrap_or(u32::MAX);
                    if number < request.line_start.unwrap_or(1) || number > request.line_end.unwrap_or(u32::MAX) { continue; }
                    let context = kind == "context";
                    if rows.get(&number).is_some_and(|r: &GrepEngineMatch| !r.is_context || context) { continue; }
                    let mut text = physical.strip_suffix('\r').unwrap_or(physical).to_owned(); let mut truncated = false;
                    if let Some(max) = request.max_columns && text.chars().count() > max as usize { text = format!("{}...",text.chars().take(max as usize).collect::<String>()); truncated = true; }
                    let column = if !context && index == 0 { data["submatches"][0]["start"].as_u64().and_then(|n| u32::try_from(n+1).ok()) } else { None };
                    rows.insert(number,GrepEngineMatch { path:display.into(),line:number,column,text,is_context:context,truncated });
                }
            }
            _ => return Err(GrepEngineError::EngineUnavailable("Invalid ripgrep JSON event".into())),
        }
    }
    Ok((rows.into_values().collect(),binary))
}
