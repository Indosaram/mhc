use super::enumerate::Candidate;
use crate::grep::engine::*;
use base64::Engine;
use serde_json::Value;
use std::{collections::BTreeMap, path::Path};

pub struct FileRows<'a> {
    pub candidate: &'a Candidate,
    pub rows: BTreeMap<u32, GrepEngineMatch>,
}

/// Rows grouped by candidate display path, plus the display paths rg reported as binary.
pub struct SegmentRows<'a> {
    pub files: BTreeMap<String, FileRows<'a>>,
    pub segment_binary: std::collections::BTreeSet<String>,
}

fn malformed(message: impl std::fmt::Display) -> GrepEngineError {
    GrepEngineError::EngineUnavailable(format!("Malformed ripgrep JSON: {message}"))
}

pub fn parse_rg_event(line: &str) -> Result<Value, GrepEngineError> {
    let event: Value = serde_json::from_str(line).map_err(malformed)?;
    let valid = event["type"]
        .as_str()
        .is_some_and(|kind| ["begin", "match", "context", "end", "summary"].contains(&kind))
        && event.get("data").is_some_and(|data| !data.is_null());
    if !valid {
        return Err(malformed("Error: Invalid ripgrep JSON event"));
    }
    Ok(event)
}

fn decode(value: &Value) -> Result<String, GrepEngineError> {
    if let Some(text) = value["text"].as_str() {
        return Ok(text.into());
    }
    if let Some(bytes) = value["bytes"].as_str() {
        return base64::engine::general_purpose::STANDARD
            .decode(bytes)
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .map_err(|e| malformed(format!("Error: {e}")));
    }
    Err(malformed("Error: Invalid ripgrep JSON text/bytes field"))
}

/// senpi `collectSegmentRows` over one completed rg launch's JSON output.
pub fn collect_segment_rows<'a>(
    output: &[u8],
    segment: &[&'a Candidate],
    request: &GrepEngineRequest,
    oversized: bool,
) -> Result<SegmentRows<'a>, GrepEngineError> {
    let first = segment[0];
    let by_path: BTreeMap<&Path, &'a Candidate> = segment
        .iter()
        .map(|candidate| (candidate.absolute.as_path(), *candidate))
        .collect();
    let mut collected = SegmentRows {
        files: BTreeMap::new(),
        segment_binary: Default::default(),
    };
    for line in String::from_utf8_lossy(output).lines() {
        let event = parse_rg_event(line)?;
        let kind = event["type"].as_str().unwrap_or_default();
        if kind == "summary" {
            continue;
        }
        let data = &event["data"];
        let candidate = if oversized {
            first
        } else {
            let path = super::enumerate::resolve(&first.root.cwd, &decode(&data["path"])?);
            *by_path.get(path.as_path()).ok_or_else(|| {
                malformed("Error: ripgrep searched a file outside the ordered segment")
            })?
        };
        match kind {
            "begin" => {
                collected.files.insert(
                    candidate.display.clone(),
                    FileRows {
                        candidate,
                        rows: BTreeMap::new(),
                    },
                );
            }
            "end" => {
                if !data["binary_offset"].is_null() {
                    collected.files.remove(&candidate.display);
                    collected.segment_binary.insert(candidate.display.clone());
                }
            }
            _ => {
                let (Some(file), Some(first_line)) = (
                    collected.files.get_mut(&candidate.display),
                    data["line_number"].as_u64(),
                ) else {
                    return Err(malformed(
                        "Error: ripgrep match/context without a begin or line number",
                    ));
                };
                let text = decode(&data["lines"])?;
                let text = text.strip_suffix('\n').unwrap_or(&text);
                for (index, physical) in text.split('\n').enumerate() {
                    let number =
                        u32::try_from(first_line.saturating_add(index as u64)).unwrap_or(u32::MAX);
                    if number < request.line_start.unwrap_or(1)
                        || number > request.line_end.unwrap_or(u32::MAX)
                    {
                        continue;
                    }
                    let is_context = kind == "context";
                    if file
                        .rows
                        .get(&number)
                        .is_some_and(|row| !row.is_context || is_context)
                    {
                        continue;
                    }
                    let column = if !is_context && index == 0 {
                        data["submatches"][0]["start"]
                            .as_u64()
                            .and_then(|n| u32::try_from(n + 1).ok())
                    } else {
                        None
                    };
                    let mut content = physical.strip_suffix('\r').unwrap_or(physical).to_owned();
                    let mut truncated = false;
                    if let Some(max) = request.max_columns
                        && content.chars().count() > max as usize
                    {
                        content = format!(
                            "{}...",
                            content.chars().take(max as usize).collect::<String>()
                        );
                        truncated = true;
                    }
                    file.rows.insert(
                        number,
                        GrepEngineMatch {
                            path: candidate.display.clone(),
                            line: number,
                            column,
                            text: content,
                            is_context,
                            truncated,
                        },
                    );
                }
            }
        }
    }
    Ok(collected)
}
