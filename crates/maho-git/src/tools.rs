//! Read-only git inspection tools on the maho extension API (`<builtin:git>`): `git_overview`,
//! `git_file_diff`, `git_hunk` and `recent_commits`. Ported from the MIT oh-my-pi agentic-commit
//! tools (`commit/agentic/tools/{git-overview,git-file-diff,git-hunk,recent-commits}.ts`) without
//! the commit-agent state (scope candidates, diff cache); they never write to the repository.
use crate::diff::{is_lock_file, parse_diff_hunks, render_file_diffs, render_stat};
use crate::repo::Git;
use maho_ext_api::{
    Extension, ExtensionApi, ToolCall, ToolContent, ToolDefinition, ToolError, ToolResult,
};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Tools this extension registers.
pub const TOOL_NAMES: &[&str] = &[
    "git_overview",
    "git_file_diff",
    "git_hunk",
    "recent_commits",
];

/// The git tools extension.
#[derive(Clone, Debug, Default)]
pub struct GitToolsExtension;

fn result(text: String, details: Value) -> ToolResult {
    ToolResult {
        content: vec![ToolContent::text(text)],
        details: Some(details),
        structured_content: None,
        is_error: None,
    }
}

fn repository(cwd: &Path) -> Result<Git, ToolError> {
    let git = Git::new(cwd);
    let root = git
        .repo_root()
        .map_err(|_| ToolError::Message(format!("not a git repository: {}", cwd.display())))?;
    Ok(Git::new(root))
}

fn staged(params: &Value) -> bool {
    params
        .get("staged")
        .and_then(Value::as_bool)
        .unwrap_or(true)
}

fn message(error: impl std::fmt::Display) -> ToolError {
    ToolError::Message(error.to_string())
}

/// `git_overview`: changed files, a stat summary and numstat rows (lock files excluded).
///
/// # Errors
/// Outside a git repository or when git fails.
pub fn overview(cwd: &Path, params: &Value) -> Result<ToolResult, ToolError> {
    let git = repository(cwd)?;
    let staged = staged(params);
    let all = git.changed_files(staged).map_err(message)?;
    let (excluded, files): (Vec<String>, Vec<String>) =
        all.into_iter().partition(|file| is_lock_file(file));
    let numstat_all = git.numstat(staged).map_err(message)?;
    let stat = render_stat(&numstat_all);
    let numstat: Vec<Value> = numstat_all
        .iter()
        .filter(|entry| !is_lock_file(&entry.path))
        .map(|entry| {
            json!({"path": entry.path, "additions": entry.added.unwrap_or(0), "deletions": entry.removed.unwrap_or(0)})
        })
        .collect();
    let mut snapshot = Map::new();
    snapshot.insert("files".into(), json!(files));
    snapshot.insert("stat".into(), json!(stat));
    snapshot.insert("numstat".into(), json!(numstat));
    if !staged && params.get("include_untracked").and_then(Value::as_bool) == Some(true) {
        snapshot.insert(
            "untrackedFiles".into(),
            json!(git.untracked().map_err(message)?),
        );
    }
    if !excluded.is_empty() {
        snapshot.insert("excludedFiles".into(), json!(excluded));
    }
    let snapshot = Value::Object(snapshot);
    Ok(result(
        serde_json::to_string_pretty(&snapshot).unwrap_or_default(),
        snapshot,
    ))
}

/// `git_file_diff`: diffs of up to ten files, highest-priority first, within the size budget.
///
/// # Errors
/// Bad parameters, outside a git repository or when git fails.
pub fn file_diff(cwd: &Path, params: &Value) -> Result<ToolResult, ToolError> {
    let files: Vec<String> = params
        .get("files")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    if files.is_empty() || files.len() > 10 {
        return Err(ToolError::Message("`files` must name 1 to 10 files".into()));
    }
    let git = repository(cwd)?;
    let staged = staged(params);
    let mut diffs = BTreeMap::new();
    for file in &files {
        let diff = git.diff_text(staged, false, &[file]).map_err(message)?;
        diffs.insert(file.clone(), diff);
    }
    let (text, truncated) = render_file_diffs(&files, |file| diffs.get(file).cloned());
    let text = if text.is_empty() {
        "(no diff)".to_owned()
    } else {
        text
    };
    let mut details = json!({"files": files, "staged": staged});
    if !truncated.is_empty() {
        details["truncatedFiles"] = json!(truncated);
    }
    Ok(result(text, details))
}

/// `git_hunk`: selected (1-based) hunks of one file's diff, all when none are named.
///
/// # Errors
/// Bad parameters, outside a git repository or when git fails.
pub fn hunk(cwd: &Path, params: &Value) -> Result<ToolResult, ToolError> {
    let file = params
        .get("file")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|f| !f.is_empty())
        .ok_or_else(|| ToolError::Message("Missing required parameter: file".into()))?
        .to_owned();
    let git = repository(cwd)?;
    let staged = staged(params);
    let diff = git.diff_text(staged, false, &[&file]).map_err(message)?;
    let parsed = parse_diff_hunks(&diff);
    let Some(file_hunks) = parsed.into_iter().find(|entry| entry.filename == file) else {
        return Ok(result(
            "(no matching hunks)".into(),
            json!({"file": file, "staged": staged, "hunks": []}),
        ));
    };
    if file_hunks.is_binary {
        return Ok(result(
            "Binary file diff; no hunks available.".into(),
            json!({"file": file, "staged": staged, "hunks": []}),
        ));
    }
    let wanted: Vec<usize> = params
        .get("hunks")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_f64)
                .map(|n| if n < 1.0 { 1 } else { n.floor() as usize })
                .collect()
        })
        .unwrap_or_default();
    let selected: Vec<_> = file_hunks
        .hunks
        .into_iter()
        .filter(|h| wanted.is_empty() || wanted.contains(&(h.index + 1)))
        .collect();
    let text = if selected.is_empty() {
        "(no matching hunks)".to_owned()
    } else {
        selected
            .iter()
            .map(|h| h.content.as_str())
            .collect::<Vec<_>>()
            .join("\n\n")
    };
    Ok(result(
        text,
        json!({"file": file, "staged": staged, "hunks": selected}),
    ))
}

fn summary_of(subject: &str) -> &str {
    // `type(scope): summary` or `type: summary`.
    let Some(colon) = subject.find(": ") else {
        return subject.trim();
    };
    let head = &subject[..colon];
    let kind_end = head.find('(').unwrap_or(head.len());
    let kind = &head[..kind_end];
    let scope_ok = kind_end == head.len() || (head.ends_with(')') && head.len() > kind_end + 2);
    if !kind.is_empty() && kind.chars().all(|c| c.is_ascii_alphabetic()) && scope_ok {
        subject[colon + 2..].trim()
    } else {
        subject.trim()
    }
}

fn scope_of(subject: &str) -> Option<&str> {
    let colon = subject.find("):")?;
    let open = subject[..colon].find('(')?;
    let kind = &subject[..open];
    if kind.is_empty() || !kind.chars().all(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    let scope = subject[open + 1..colon].trim();
    (!scope.is_empty()).then_some(scope)
}

/// `recent_commits`: the last subjects with style statistics.
///
/// # Errors
/// Outside a git repository or when git fails.
pub fn recent_commits(cwd: &Path, params: &Value) -> Result<ToolResult, ToolError> {
    let count = params
        .get("count")
        .and_then(Value::as_f64)
        .map_or(8, |n| n.clamp(1.0, 50.0) as usize);
    let git = repository(cwd)?;
    let commits = git.log_subjects(count).map_err(message)?;
    let mut verbs: BTreeMap<String, u64> = BTreeMap::new();
    let mut scopes: BTreeMap<String, u64> = BTreeMap::new();
    let mut lengths = Vec::new();
    let mut scoped = 0u64;
    let mut lowercase = 0u64;
    for subject in &commits {
        let summary = summary_of(subject);
        if let Some(scope) = scope_of(subject) {
            scoped += 1;
            *scopes.entry(scope.to_owned()).or_default() += 1;
        }
        if let Some(first) = summary.chars().next()
            && first.to_lowercase().eq(std::iter::once(first))
        {
            lowercase += 1;
        }
        if let Some(word) = summary.split_whitespace().next() {
            *verbs.entry(word.to_lowercase()).or_default() += 1;
        }
        lengths.push(summary.chars().count());
    }
    let total = commits.len();
    let percent = |n: u64| {
        if total == 0 {
            0
        } else {
            (n as f64 * 100.0 / total as f64).round() as u64
        }
    };
    let average = if lengths.is_empty() {
        0.0
    } else {
        (lengths.iter().sum::<usize>() as f64 / lengths.len() as f64 * 10.0).round() / 10.0
    };
    let payload = json!({
        "commits": commits,
        "stats": {
            "scopeUsagePercent": percent(scoped),
            "commonVerbs": verbs,
            "summaryLength": {
                "min": lengths.iter().min().copied().unwrap_or(0),
                "max": lengths.iter().max().copied().unwrap_or(0),
                "average": average,
            },
            "lowercaseSummaryPercent": percent(lowercase),
            "topScopes": scopes,
        }
    });
    Ok(result(
        serde_json::to_string_pretty(&payload).unwrap_or_default(),
        payload,
    ))
}

type Run = fn(&Path, &Value) -> Result<ToolResult, ToolError>;

fn define(
    name: &str,
    label: &str,
    description: &str,
    parameters: Value,
    cwd: PathBuf,
    run: Run,
) -> ToolDefinition {
    let mut definition = ToolDefinition::new(
        name,
        description,
        parameters,
        Arc::new(move |call: ToolCall<'_>| {
            let cwd = call
                .context
                .map_or_else(|| cwd.clone(), |context| context.cwd().to_path_buf());
            let params = call.params.clone();
            Box::pin(async move {
                tokio::task::spawn_blocking(move || run(&cwd, &params))
                    .await
                    .map_err(|error| ToolError::Message(format!("git tool failed: {error}")))?
            })
        }),
    );
    label.clone_into(&mut definition.label);
    definition
}

/// The four tool definitions bound to `cwd` (a call's own context cwd wins).
pub fn definitions(cwd: &Path) -> Vec<ToolDefinition> {
    let staged = json!({"type": "boolean", "description": "Use staged changes (default true); false = unstaged working-tree changes"});
    vec![
        define(
            "git_overview",
            "Git Overview",
            "Summarise the repository's staged (default) or unstaged changes: changed files, a diff stat and per-file added/removed line counts. Lock files are listed separately. Read-only.",
            json!({"type": "object", "properties": {"staged": staged, "include_untracked": {"type": "boolean", "description": "With staged=false, also list untracked files"}}}),
            cwd.to_path_buf(),
            overview,
        ),
        define(
            "git_file_diff",
            "Git File Diff",
            "Show the staged (default) or unstaged diff of 1-10 files, most important files first; long diffs are shortened. Read-only.",
            json!({"type": "object", "properties": {"files": {"type": "array", "items": {"type": "string"}, "minItems": 1, "maxItems": 10, "description": "Repository-relative paths"}, "staged": staged}, "required": ["files"]}),
            cwd.to_path_buf(),
            file_diff,
        ),
        define(
            "git_hunk",
            "Git Hunk",
            "Show specific hunks (1-based) of one file's staged (default) or unstaged diff; all hunks when none are named. Read-only.",
            json!({"type": "object", "properties": {"file": {"type": "string"}, "hunks": {"type": "array", "items": {"type": "number"}, "minItems": 1}, "staged": staged}, "required": ["file"]}),
            cwd.to_path_buf(),
            hunk,
        ),
        define(
            "recent_commits",
            "Recent Commits",
            "List recent commit subjects with style statistics (conventional scopes, verbs, summary length, casing) to match the project's commit style. Read-only.",
            json!({"type": "object", "properties": {"count": {"type": "number", "minimum": 1, "maximum": 50, "description": "Number of commits (default 8)"}}}),
            cwd.to_path_buf(),
            recent_commits,
        ),
    ]
}

impl Extension for GitToolsExtension {
    fn register(&self, api: &mut ExtensionApi) {
        for definition in definitions(&api.cwd) {
            if let Err(error) = api.try_register_tool(definition) {
                maho_ext_api::diagnostic(&format!("git: {error}"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{scope_of, summary_of};

    #[test]
    fn conventional_subjects() {
        assert_eq!(summary_of("feat(cli): add flag"), "add flag");
        assert_eq!(summary_of("fix: typo"), "typo");
        assert_eq!(summary_of("Merge branch: x"), "Merge branch: x");
        assert_eq!(summary_of("plain subject"), "plain subject");
        assert_eq!(scope_of("feat(cli): add"), Some("cli"));
        assert_eq!(scope_of("fix: x"), None);
    }
}
