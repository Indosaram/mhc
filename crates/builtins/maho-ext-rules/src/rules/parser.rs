use super::types::{ParsedRule, RuleFrontmatter};
pub fn parse_rule(content: &str) -> ParsedRule {
 let normalized = content.strip_prefix('﻿').unwrap_or(content);
 let opening = if normalized.starts_with("---\r\n") { 5 } else if normalized.starts_with("---\n") { 4 } else { 0 };
 if opening == 0 { return ParsedRule { frontmatter: RuleFrontmatter::default(), body: normalized.into(), diagnostic: None }; }
 let mut start = opening;
 for line in normalized[opening..].split_inclusive('\n') {
  if line.trim_end_matches('\n').trim_end_matches('\r') == "---" {
   let body_start = start.saturating_add(line.len());
   return match parse_yaml_frontmatter(&normalized[opening..start]) {
    Ok(frontmatter) => ParsedRule { frontmatter, body: normalized[body_start..].into(), diagnostic: None },
    Err(message) => ParsedRule { frontmatter: RuleFrontmatter::default(), body: normalized.into(), diagnostic: Some(format!("Malformed frontmatter: {message}")) },
   };
  }
  start = start.saturating_add(line.len());
 }
 ParsedRule { frontmatter: RuleFrontmatter::default(), body: normalized.into(), diagnostic: Some("Missing closing frontmatter delimiter".into()) }
}
fn strip_comment(line: &str) -> &str {
 let mut quote = None; let mut escaped = false;
 for (offset, character) in line.char_indices() {
  if escaped { escaped = false; continue; }
  if quote.is_some() && character == '\\' { escaped = true; continue; }
  if character == '"' || character == '\'' { if quote.is_none() { quote = Some(character); } else if quote == Some(character) { quote = None; } continue; }
  if quote.is_none() && character == '#' { return &line[..offset]; }
 }
 line
}
fn parse_string(value: &str) -> Result<String, String> {
 if value.starts_with('"') { return serde_json::from_str(value).map_err(|_| "Invalid JSON-quoted string".into()); }
 if value.starts_with('\'') { return value.strip_prefix('\'').and_then(|value| value.strip_suffix('\'')).map(str::to_owned).ok_or_else(|| "Unclosed quoted value".into()); }
 Ok(value.into())
}
fn split_comma(value: &str) -> Result<Vec<String>, String> {
 let mut quote = None; let mut escaped = false; let mut start = 0; let mut result = Vec::new();
 for (offset, character) in value.char_indices() {
  if escaped { escaped = false; continue; }
  if quote.is_some() && character == '\\' { escaped = true; continue; }
  if character == '"' || character == '\'' { if quote.is_none() { quote = Some(character); } else if quote == Some(character) { quote = None; } continue; }
  if quote.is_none() && character == ',' { let item = value[start..offset].trim(); if !item.is_empty() { result.push(parse_string(item)?); } start = offset.saturating_add(1); }
 }
 if quote.is_some() { return Err("Unclosed quoted value".into()); }
 let last = value[start..].trim(); if !last.is_empty() { result.push(parse_string(last)?); }
 Ok(result.into_iter().filter(|value| !value.is_empty()).collect())
}
fn parse_yaml_frontmatter(content: &str) -> Result<RuleFrontmatter, String> {
 let normalized = content.replace("\r\n", "\n"); let lines: Vec<&str> = normalized.split('\n').collect(); let mut index = 0; let mut result = RuleFrontmatter::default();
 while let Some(raw) = lines.get(index) {
  let line = strip_comment(raw).trim();
  if line.is_empty() { index = index.saturating_add(1); continue; }
  let Some((key, value)) = line.split_once(':') else { return Err(format!("Expected key-value pair on line {}", index.saturating_add(1))); };
  let value = value.trim();
  match key.trim() {
   "description" => result.description = Some(parse_string(value)?),
   "alwaysApply" => result.always_apply = Some(match value { "true" => true, "false" => false, _ => return Err(format!("Expected boolean on line {}", index.saturating_add(1))) }),
   "globs" | "paths" | "applyTo" => {
    let values = if value.starts_with('[') {
     let mut quote = None; let mut escaped = false; let mut closing = None;
     for (offset, character) in value.char_indices().skip(1) {
      if escaped { escaped = false; continue; }
      if quote.is_some() && character == '\\' { escaped = true; continue; }
      if character == '"' || character == '\'' { if quote.is_none() { quote = Some(character); } else if quote == Some(character) { quote = None; } continue; }
      if quote.is_none() && character == ']' { closing = Some(offset); break; }
     }
     let closing = closing.ok_or("Unclosed inline array")?;
     if !value[closing.saturating_add(1)..].trim().is_empty() { return Err("Unexpected content after inline array".into()); }
     split_comma(&value[1..closing])?
    } else if value.is_empty() {
     let mut values = Vec::new();
     while let Some(next) = lines.get(index.saturating_add(1)) {
      let uncommented = strip_comment(next);
      if uncommented.trim().is_empty() { index = index.saturating_add(1); continue; }
      if !uncommented.starts_with(char::is_whitespace) { break; }
      let Some(item) = uncommented.trim_start().strip_prefix('-') else { break; };
      let parsed = parse_string(item.trim_start())?; if !parsed.is_empty() { values.push(parsed); } index = index.saturating_add(1);
     }
     values
    } else { let parsed = parse_string(value)?; if parsed.contains(',') { parsed.split(',').map(str::trim).filter(|part| !part.is_empty()).map(str::to_owned).collect() } else { vec![parsed] } };
    for value in values { if !result.globs.contains(&value) { result.globs.push(value); } }
   }
   _ => {}
  }
  index = index.saturating_add(1);
 }
 Ok(result)
}
