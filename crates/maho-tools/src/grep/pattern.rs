use super::engine::*;
use crate::definition::AbortSignal;
pub fn recover_pattern(pattern: &str) -> String {
    let chars: Vec<_> = pattern.chars().collect();
    let mut escape = std::collections::BTreeSet::new(); let mut stack = Vec::new(); let mut class = false; let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' { i += 2; continue; }
        if c == '[' { class = true; }
        if c == ']' { class = false; }
        if class { i += 1; continue; }
        if c == '{' {
            let mut end = i+1;
            while end < chars.len() && chars[end].is_ascii_digit() { end += 1; }
            let digits = end > i+1;
            if end < chars.len() && chars[end] == ',' { end += 1; while end < chars.len() && chars[end].is_ascii_digit() { end += 1; } }
            if digits && end < chars.len() && chars[end] == '}' { i = end+1; continue; }
        }
        if c == '{' || c == '}' { escape.insert(i); }
        if c == '(' { stack.push(i); }
        if c == ')' && stack.pop().is_none() { escape.insert(i); }
        i += 1;
    }
    escape.extend(stack); let mut result = String::new();
    for (i,c) in chars.into_iter().enumerate() { if escape.contains(&i) { result.push('\\'); } result.push(c); } result
}
async fn search(request: GrepEngineRequest, signal: &AbortSignal) -> Result<GrepEngineResult, GrepEngineError> {
    if std::env::var("SENPI_GREP_ENGINE").as_deref() == Ok("rg") { return super::rg_engine::search(request,signal).await; }
    match super::native_engine::search(request.clone(),signal).await {
        Err(GrepEngineError::UnsupportedRegex(_)) => { let mut request = request; request.pcre2 = Some(true); super::rg_engine::search(request,signal).await },
        result => result,
    }
}
pub async fn search_pattern(mut request: GrepEngineRequest, signal: &AbortSignal) -> Result<GrepEngineResult, GrepEngineError> {
    let original = request.pattern.clone();
    match search(request.clone(), signal).await {
        Ok(result) => Ok(result),
        Err(GrepEngineError::InvalidPattern(_)) if request.literal != Some(true) => {
            request.pattern = recover_pattern(&original);
            match search(request.clone(), signal).await {
                Ok(mut result) => { result.pattern_kind = "sanitized".into(); Ok(result) },
                Err(GrepEngineError::InvalidPattern(_)) => {
                    request.pattern = original; request.literal = Some(true);
                    search(request, signal).await
                }
                Err(error) => Err(error),
            }
        }
        Err(error) => Err(error),
    }
}
