use std::{collections::{BTreeMap, BTreeSet}, path::PathBuf, sync::Arc, time::Instant};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use crate::{definition::*, filesystem_policy::*};
use super::{engine::*, pattern::search_pattern};
#[derive(Clone, Deserialize)]
#[serde(untagged)]
pub enum Strings { One(String), Many(Vec<String>) }
impl Strings { fn into_vec(self) -> Vec<String> { match self { Self::One(s) => vec![s], Self::Many(v) => v } } }
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrepToolInput {
    pub pattern: String, pub path: Option<Strings>, pub glob: Option<Strings>, pub r#type: Option<String>,
    pub ignore_case: Option<bool>, pub literal: Option<bool>, pub multiline: Option<bool>, pub context: Option<u32>,
    pub before: Option<u32>, pub after: Option<u32>, pub mode: Option<String>, pub limit: Option<u32>, pub skip: Option<u32>,
    pub timeout_ms: Option<u32>, pub hidden: Option<bool>, pub gitignore: Option<bool>,
}
#[derive(Clone, Default)]
pub struct GrepToolOptions { pub filesystem_policy: Option<FilesystemPolicyChecker> }
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GrepToolMatch { pub path: String, pub line: u32, #[serde(skip_serializing_if = "Option::is_none")] pub column: Option<u32>, pub text: String, pub is_context: bool, pub truncated: bool }
impl From<&GrepEngineMatch> for GrepToolMatch {
    fn from(row: &GrepEngineMatch) -> Self { Self { path: row.path.clone(), line: row.line, column: row.column, text: row.text.clone(), is_context: row.is_context, truncated: row.truncated } }
}
#[derive(Clone, Serialize)]
pub struct FileMatch { pub path: String, pub count: Option<u32> }
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GrepToolDetails {
    pub version: u8, pub engine: String, pub status: String, pub cwd: String, pub paths: Vec<String>,
    pub matches: Vec<GrepToolMatch>, pub file_matches: Vec<FileMatch>, pub match_count: Option<u32>, pub file_count: usize,
    pub skip: u32, pub next_skip: Option<u32>, pub file_limit_reached: bool, pub per_file_limit_reached: bool,
    pub total_limit_reached: bool, pub scan: Value, pub lines_truncated: bool,
}
fn scan_value(scan: &GrepEngineResult) -> Value {
    json!({"counts":{"matches":scan.counts.matches,"files":scan.counts.files,"exact":scan.counts.exact},"filesSearched":scan.files_searched,
        "limitReached":scan.limit_reached,"perFileLimitReached":scan.per_file_limit_reached,"skippedOversized":scan.skipped_oversized,
        "prefixSearched":scan.prefix_searched,"skippedBinary":scan.skipped_binary,"missingPaths":scan.missing_paths,
        "warnings":scan.warnings.iter().map(|w| json!({"path":w.path,"code":w.code,"message":w.message})).collect::<Vec<_>>(),
        "timedOut":scan.timed_out,"elapsedMs":scan.elapsed_ms,"effectivePattern":scan.effective_pattern,"patternKind":scan.pattern_kind,"regexEngine":scan.regex_engine})
}
pub fn create_grep_tool_definition(cwd: PathBuf, options: GrepToolOptions) -> ToolDefinition {
    let execute: ToolExecutor = Arc::new(move |call| {
        let cwd = cwd.clone(); let policy = options.filesystem_policy.clone();
        Box::pin(async move {
            let started = Instant::now(); let input: GrepToolInput = serde_json::from_value(call.params)?;
            if input.pattern.is_empty() { return Err(ToolError::Message("Pattern must not be empty".into())); }
            let requested = input.path.map(Strings::into_vec).unwrap_or_else(|| vec![".".into()]);
            if requested.is_empty() || requested.iter().any(String::is_empty) { return Err(ToolError::Message("path must be a non-empty string or array of non-empty strings".into())); }
            let limit = input.limit.unwrap_or(100); if limit == 0 { return Err(ToolError::Message("limit must be a positive integer".into())); }
            let cwd = call.context.map_or(cwd.as_path(), ToolContext::cwd).to_path_buf();
            let mut paths = Vec::new(); let mut missing = Vec::new(); let mut single_file = false; let mut line_start = None; let mut line_end = None;
            for raw in &requested {
                let mut path = crate::path_utils::resolve_to_cwd(raw, &cwd); let mut info = tokio::fs::metadata(&path).await;
                if info.as_ref().is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
                    && let Some((prefix, range)) = path.to_string_lossy().rsplit_once(":L")
                    && let Some((start,end)) = range.split_once('-')
                    && let (Ok(start),Ok(end)) = (start.parse::<u32>(),end.trim_start_matches('L').parse::<u32>())
                    && let Ok(meta) = tokio::fs::metadata(prefix).await
                    && meta.is_file() {
                                        if requested.len() != 1 || start == 0 || end < start { return Err(ToolError::Message("Invalid line selector".into())); }
                                        let selected = PathBuf::from(prefix); line_start = Some(start); line_end = Some(end); info = Ok(meta); path = selected;
                }
                check_filesystem_policy(policy.as_ref(), &path, FilesystemOperation::Enumerate, "grep").await?;
                match info {
                    Ok(info) => { single_file = info.is_file(); paths.push(path.to_string_lossy().into_owned()); },
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => missing.push(path.to_string_lossy().into_owned()),
                    Err(error) => return Err(error.into()),
                }
            }
            if paths.is_empty() { return Err(ToolError::Message(format!("Path not found: {}", missing.join(", ")))); }
            single_file &= paths.len() == 1;
            let skip = if single_file { 0 } else { input.skip.unwrap_or(0) }; let page_size = limit.min(20); let cap = if single_file { 200 } else { 20 };
            let mode = match input.mode.as_deref().unwrap_or("content") { "content" => GrepMode::Content, "count" => GrepMode::Count, "files" => GrepMode::Files, other => return Err(ToolError::Message(format!("Invalid grep mode: {other}"))) };
            let before = input.before.or(input.context).unwrap_or(0); let after = input.after.or(input.context).unwrap_or(0);
            let request = GrepEngineRequest { multiline: Some(input.multiline.unwrap_or_else(|| input.pattern.contains('\n') || input.pattern.contains("\\n"))), pattern: input.pattern,
                paths: paths.clone(), cwd: cwd.to_string_lossy().into_owned(), glob: input.glob.map(Strings::into_vec), r#type: input.r#type,
                ignore_case: input.ignore_case, literal: input.literal, hidden: input.hidden, gitignore: input.gitignore,
                context_before: Some(before), context_after: Some(after), max_columns: Some(500), timeout_ms: Some(input.timeout_ms.unwrap_or(30000)),
                line_start, line_end, mode: Some(mode), ..Default::default() };
            super::select_engine::resolve_grep_engine()?;
            let mut preselection = None; let mut selected = Vec::new(); let mut more = false;
            if !single_file {
                let mut first = request.clone(); first.mode = Some(GrepMode::Files); first.max_count = Some(skip.saturating_add(page_size).saturating_add(1));
                let result = search_pattern(first, &call.signal).await.map_err(|e| ToolError::Message(e.to_string()))?;
                selected = result.file_counts.iter().skip(skip as usize).take(page_size as usize).map(|f| f.path.clone()).collect();
                more = result.file_counts.len() > skip as usize + selected.len(); preselection = Some(result);
            }
            let result = if !single_file && (selected.is_empty() || mode == GrepMode::Files) {
                let mut result = preselection.clone().ok_or_else(|| ToolError::Message("Missing grep preselection".into()))?;
                if !selected.is_empty() { result.file_counts = result.file_counts.into_iter().skip(skip as usize).take(page_size as usize).collect(); } result
            } else {
                let mut second = request.clone();
                if !single_file { second.paths = selected.iter().map(|p| cwd.join(p).to_string_lossy().into_owned()).collect(); }
                second.max_count_per_file = if mode == GrepMode::Content { Some(cap+1) } else { None };
                second.timeout_ms = Some(input.timeout_ms.unwrap_or(30000).saturating_sub(u32::try_from(started.elapsed().as_millis()).unwrap_or(u32::MAX)));
                search_pattern(second, &call.signal).await.map_err(|e| ToolError::Message(e.to_string()))?
            };
            let mut matches = Vec::new(); let mut file_matches = Vec::new(); let mut per_file = false; let mut total_limit = false;
            if mode == GrepMode::Content {
                let mut groups: BTreeMap<&str,Vec<usize>> = BTreeMap::new();
                for (i,row) in result.matches.iter().enumerate().filter(|(_,r)| !r.is_context) { groups.entry(&row.path).or_default().push(i); }
                let mut admitted = BTreeSet::new();
                for round in 0..cap as usize { for rows in groups.values() { if admitted.len() < limit as usize && let Some(i) = rows.get(round) { admitted.insert(*i); } } }
                per_file = groups.values().any(|rows| rows.len() > cap as usize);
                total_limit = groups.values().map(|rows| rows.len().min(cap as usize)).sum::<usize>() > admitted.len();
                let mut bytes = 0;
                for (i,row) in result.matches.iter().enumerate() {
                    if admitted.contains(&i) || (row.is_context && admitted.iter().any(|i| { let m = &result.matches[*i]; m.path == row.path && row.line >= m.line.saturating_sub(before) && row.line <= m.line.saturating_add(after) })) {
                        bytes += format!("{}\n{}: {}\n", row.path, row.line, row.text).len();
                        if bytes > crate::truncate::DEFAULT_MAX_BYTES-2048 { total_limit = true; } else { matches.push(GrepToolMatch::from(row)); }
                    }
                }
                let mut counts = BTreeMap::new();
                for row in matches.iter().filter(|r| !r.is_context) { *counts.entry(row.path.clone()).or_insert(0u32) += 1; }
                matches.retain(|r| counts.contains_key(&r.path)); file_matches = counts.into_iter().map(|(path,count)| FileMatch { path, count: Some(count) }).collect();
            } else if single_file || !selected.is_empty() { file_matches = result.file_counts.iter().map(|f| FileMatch { path: f.path.clone(), count: f.count }).collect(); }
            let mut scan = preselection.unwrap_or_else(|| result.clone()); scan.missing_paths = missing; scan.elapsed_ms = started.elapsed().as_secs_f64()*1000.0;
            scan.effective_pattern = result.effective_pattern; scan.pattern_kind = result.pattern_kind; scan.regex_engine = result.regex_engine; scan.timed_out |= result.timed_out;
            let partial = scan.timed_out || scan.prefix_searched > 0 || scan.skipped_oversized > 0;
            let engine = if scan.regex_engine == "pcre2" || std::env::var("SENPI_GREP_ENGINE").as_deref() == Ok("rg") { "rg" } else { "native" };
            let details = GrepToolDetails { version: 1, engine: engine.into(), status: if partial { "partial" } else if !file_matches.is_empty() { "ok" } else if skip > 0 { "pageEnd" } else { "noMatch" }.into(),
                cwd: request.cwd, paths, match_count: if mode == GrepMode::Files { None } else { Some(file_matches.iter().map(|f| f.count.unwrap_or(0)).sum()) }, file_count: file_matches.len(),
                skip, next_skip: more.then_some(skip + selected.len() as u32), file_limit_reached: more, per_file_limit_reached: per_file, total_limit_reached: total_limit,
                scan: scan_value(&scan), lines_truncated: matches.iter().any(|r| r.truncated), matches, file_matches };
            Ok(ToolResult { content: super::format::format_grep_content(&details), details: Some(serde_json::to_value(details)?) })
        })
    });
    let mut tool = ToolDefinition::new("grep", "Search file contents with tool.grep({ pattern, path }) inside eval. Supports regex/literal patterns, file pages with skip, context, count/files modes, and structured results. Respects .gitignore.", json!({"type":"object","properties":{"pattern":{"type":"string"},"path":{"oneOf":[{"type":"string"},{"type":"array","items":{"type":"string"}}]},"limit":{"type":"number"},"skip":{"type":"number"}},"required":["pattern"]}), execute);
    tool.parameters["properties"]["glob"] = json!({"oneOf":[{"type":"string"},{"type":"array","items":{"type":"string"}}]});
    for name in ["ignoreCase","literal","multiline","hidden","gitignore"] { tool.parameters["properties"][name] = json!({"type":"boolean"}); }
    for name in ["context","before","after","timeoutMs"] { tool.parameters["properties"][name] = json!({"type":"number"}); }
    tool.parameters["properties"]["type"] = json!({"type":"string"});
    tool.parameters["properties"]["mode"] = json!({"type":"string","enum":["content","count","files"]});
    tool.prompt_guidelines = Some(vec!["Use tool.grep for content search instead of rg/grep in a shell; paginate with skip and narrow with glob/path when a page is full".into()]);
    tool.exposure = Some(ToolExposure::Eval); tool.prompt_snippet = Some("Search file contents (regex/literal) across paths with structured results; respects .gitignore".into()); tool
}
