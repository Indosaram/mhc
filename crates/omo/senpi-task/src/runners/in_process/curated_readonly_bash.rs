//! Structured read-only `gh`/`curl` replacement for `bash` in curated agents
//! (`runners/in-process/curated-readonly-bash.ts`).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use crate::host::HostError;
use crate::runners::in_process::shared_tool_filter::ChildTool;

use serde_json::{Value, json};
use utils::process_tree::{ProcessTreeRunOptions, run_process_with_tree_timeout};

/// senpi's `DEFAULT_MAX_LINES` / `DEFAULT_MAX_BYTES` tool-output budget.
pub const DEFAULT_MAX_LINES: usize = 2000;
pub const DEFAULT_MAX_BYTES: usize = 50 * 1024;
const MAX_BUFFER: usize = 512 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadonlyProgram {
    Curl,
    Gh,
}

impl ReadonlyProgram {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Curl => "curl",
            Self::Gh => "gh",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CuratedReadonlyCommand {
    pub program: ReadonlyProgram,
    pub args: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CuratedReadonlyBashInput {
    pub program: ReadonlyProgram,
    pub args: Vec<String>,
    pub timeout_seconds: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct CuratedReadonlyCommandError(pub String);

pub type CuratedReadonlyExecutor = Arc<
    dyn Fn(&CuratedReadonlyCommand, &str, u64) -> Result<String, CuratedReadonlyCommandError>
        + Send
        + Sync,
>;

const CURL_BOOLEAN_FLAGS: &[&str] = &[
    "-f",
    "--fail",
    "--fail-with-body",
    "-I",
    "--head",
    "-L",
    "--location",
    "--compressed",
    "--no-progress-meter",
    "-s",
    "--silent",
    "-S",
    "--show-error",
];
const CURL_VALUE_FLAGS: &[&str] = &[
    "--connect-timeout",
    "--max-time",
    "--retry",
    "--retry-delay",
    "-A",
    "--user-agent",
];
const GH_SEARCH_KINDS: &[&str] = &["code", "commits", "issues", "prs", "repos"];
const GH_SEARCH_BOOLEAN_FLAGS: &[&str] = &["--archived", "--include-forks"];
const GH_SEARCH_VALUE_FLAGS: &[&str] = &[
    "--extension",
    "--filename",
    "--language",
    "--limit",
    "--match",
    "--order",
    "--owner",
    "--repo",
    "--sort",
    "--state",
    "--updated",
    "--visibility",
    "--json",
    "--jq",
    "--template",
];
const GH_VIEW_BOOLEAN_FLAGS: &[&str] = &["--comments"];
const GH_VIEW_VALUE_FLAGS: &[&str] = &["--json", "--jq", "--repo", "--template"];
const GH_API_BOOLEAN_FLAGS: &[&str] = &["--include", "--paginate", "--slurp"];
const GH_API_VALUE_FLAGS: &[&str] = &["--hostname", "--jq", "--template"];

pub fn plan_curated_readonly_command(
    program: ReadonlyProgram,
    args: &[String],
) -> Result<CuratedReadonlyCommand, CuratedReadonlyCommandError> {
    match program {
        ReadonlyProgram::Curl if is_readonly_curl(args) => {
            let args = if args.first().map(String::as_str) == Some("--version") {
                args.to_vec()
            } else {
                std::iter::once("--disable".to_string())
                    .chain(args.iter().cloned())
                    .collect()
            };
            Ok(CuratedReadonlyCommand { program, args })
        }
        ReadonlyProgram::Gh if is_readonly_github(args) => Ok(CuratedReadonlyCommand {
            program,
            args: args.to_vec(),
        }),
        _ => Err(CuratedReadonlyCommandError(
            "The curated bash tool accepts read-only GitHub and HTTPS retrieval operations only."
                .to_string(),
        )),
    }
}

fn is_version(args: &[String]) -> bool {
    args.len() == 1 && args[0] == "--version"
}

fn is_readonly_curl(args: &[String]) -> bool {
    if is_version(args) {
        return true;
    }
    let mut urls = 0;
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        if arg.starts_with("https://") {
            urls += 1;
        } else if !CURL_BOOLEAN_FLAGS.contains(&arg) {
            if !CURL_VALUE_FLAGS.contains(&arg) || index + 1 >= args.len() {
                return false;
            }
            index += 1;
        }
        index += 1;
    }
    urls == 1
}

fn is_readonly_github(args: &[String]) -> bool {
    if is_version(args) {
        return true;
    }
    let command = args.first().map(String::as_str);
    let subcommand = args.get(1).map(String::as_str);
    let rest = args.get(2..).unwrap_or_default();
    match (command, subcommand) {
        (Some("search"), Some(kind)) if GH_SEARCH_KINDS.contains(&kind) => {
            flags_and_positionals_are_safe(rest, GH_SEARCH_BOOLEAN_FLAGS, GH_SEARCH_VALUE_FLAGS, 1)
        }
        (Some("repo"), Some("view")) | (Some("release"), Some("view" | "list")) => {
            flags_and_positionals_are_safe(rest, &[], GH_VIEW_VALUE_FLAGS, 1)
        }
        (Some("issue" | "pr"), Some("view")) => {
            flags_and_positionals_are_safe(rest, GH_VIEW_BOOLEAN_FLAGS, GH_VIEW_VALUE_FLAGS, 1)
        }
        (Some("api"), Some(endpoint)) if endpoint != "graphql" && !endpoint.starts_with('-') => {
            flags_and_positionals_are_safe(rest, GH_API_BOOLEAN_FLAGS, GH_API_VALUE_FLAGS, 0)
        }
        _ => false,
    }
}

fn flags_and_positionals_are_safe(
    args: &[String],
    boolean_flags: &[&str],
    value_flags: &[&str],
    max_positionals: usize,
) -> bool {
    let mut positionals = 0;
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        if !arg.starts_with('-') {
            positionals += 1;
            if positionals > max_positionals {
                return false;
            }
        } else if !boolean_flags.contains(&arg) {
            if !value_flags.contains(&arg) || index + 1 >= args.len() {
                return false;
            }
            index += 1;
        }
        index += 1;
    }
    true
}

/// The `bash` tool override: plan, execute via the executor, then cap the result text.
pub struct CuratedReadonlyBashTool {
    cwd: String,
    executor: CuratedReadonlyExecutor,
}

impl CuratedReadonlyBashTool {
    pub const NAME: &'static str = "bash";

    pub fn new(cwd: &str, executor: Option<CuratedReadonlyExecutor>) -> Self {
        Self {
            cwd: cwd.to_string(),
            executor: executor.unwrap_or_else(|| Arc::new(execute_command)),
        }
    }

    pub fn execute(
        &self,
        input: &CuratedReadonlyBashInput,
    ) -> Result<String, CuratedReadonlyCommandError> {
        let command = plan_curated_readonly_command(input.program, &input.args)?;
        (self.executor)(&command, &self.cwd, input.timeout_seconds.unwrap_or(30))
            .map(|text| cap_result_text(&text))
            .map_err(|error| CuratedReadonlyCommandError(cap_result_text(&error.0)))
    }
}

const DESCRIPTION: &str = "Run a structured read-only gh or curl request directly, without a shell or filesystem-writing flags.";

/// Boundary parse of the tool input JSON (`CuratedReadonlyBashParams`).
fn parse_input(input: &Value) -> Result<CuratedReadonlyBashInput, String> {
    let program = match input.get("program").and_then(Value::as_str) {
        Some("curl") => ReadonlyProgram::Curl,
        Some("gh") => ReadonlyProgram::Gh,
        _ => return Err("program must be \"curl\" or \"gh\"".to_string()),
    };
    let args: Vec<String> = input
        .get("args")
        .and_then(Value::as_array)
        .ok_or("args must be an array of strings")?
        .iter()
        .map(|arg| {
            arg.as_str()
                .map(str::to_string)
                .ok_or("args must be strings")
        })
        .collect::<Result<_, _>>()?;
    if args.is_empty() || args.len() > 64 {
        return Err("args must hold 1 to 64 entries".to_string());
    }
    let timeout_seconds = match input.get("timeout_seconds") {
        None | Some(Value::Null) => None,
        Some(value) => Some(
            value
                .as_u64()
                .filter(|seconds| (1..=120).contains(seconds))
                .ok_or("timeout_seconds must be an integer from 1 to 120")?,
        ),
    };
    Ok(CuratedReadonlyBashInput {
        program,
        args,
        timeout_seconds,
    })
}

impl ChildTool for CuratedReadonlyBashTool {
    fn name(&self) -> &str {
        Self::NAME
    }

    fn description(&self) -> &str {
        DESCRIPTION
    }

    fn execute(&self, _tool_call_id: &str, input: &Value) -> Result<Value, HostError> {
        let input = parse_input(input).map_err(|message| HostError { message })?;
        let text = CuratedReadonlyBashTool::execute(self, &input)
            .map_err(|error| HostError { message: error.0 })?;
        Ok(json!({ "content": [{ "type": "text", "text": text }] }))
    }
}

struct HeadTruncation {
    content: String,
    truncated: bool,
    by_lines: bool,
    total_lines: usize,
    total_bytes: usize,
    first_line_exceeds_limit: bool,
}

fn split_lines_for_counting(content: &str) -> Vec<&str> {
    if content.is_empty() {
        return Vec::new();
    }
    let mut lines: Vec<&str> = content.split('\n').collect();
    if content.ends_with('\n') {
        lines.pop();
    }
    lines
}

/// senpi `truncateHead`: keep whole leading lines within both budgets.
fn truncate_head(content: &str, max_bytes: usize, max_lines: usize) -> HeadTruncation {
    let total_bytes = content.len();
    let lines = split_lines_for_counting(content);
    let total_lines = lines.len();
    let result = |content: String, truncated, by_lines, first_line_exceeds_limit| HeadTruncation {
        content,
        truncated,
        by_lines,
        total_lines,
        total_bytes,
        first_line_exceeds_limit,
    };
    if total_lines <= max_lines && total_bytes <= max_bytes {
        return result(content.to_string(), false, false, false);
    }
    if lines[0].len() > max_bytes {
        return result(String::new(), true, false, true);
    }
    let mut kept: Vec<&str> = Vec::new();
    let mut bytes = 0;
    let mut by_lines = true;
    for (index, line) in lines.iter().take(max_lines).enumerate() {
        let line_bytes = line.len() + usize::from(index > 0);
        if bytes + line_bytes > max_bytes {
            by_lines = false;
            break;
        }
        kept.push(line);
        bytes += line_bytes;
    }
    if kept.len() >= max_lines && bytes <= max_bytes {
        by_lines = true;
    }
    result(kept.join("\n"), true, by_lines, false)
}

/// senpi `formatSize`.
fn format_size(bytes: usize) -> String {
    let value = bytes as f64;
    if bytes < 1024 {
        format!("{bytes}B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1}KB", value / 1024.0)
    } else {
        format!("{:.1}MB", value / (1024.0 * 1024.0))
    }
}

fn count_lines(text: &str) -> usize {
    if text.is_empty() {
        0
    } else {
        text.split('\n').count()
    }
}

fn truncation_notice(
    by_lines: bool,
    content: &str,
    total_lines: usize,
    total_bytes: usize,
) -> String {
    let shown = if by_lines {
        format!("{} of {total_lines} lines", count_lines(content))
    } else {
        format!(
            "{} of {}",
            format_size(content.len()),
            format_size(total_bytes)
        )
    };
    format!("[truncated: {shown}. Narrow the request before retrying.]")
}

fn truncate_utf8_prefix(text: &str, max_bytes: usize) -> String {
    let mut end = max_bytes.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

/// Cap to the tool budget; the notice size depends on the kept content, so iterate to a fixpoint.
fn cap_result_text(text: &str) -> String {
    let boundary = truncate_head(text, DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES);
    if !boundary.truncated {
        return text.to_string();
    }
    let notice_for = |content: &str| {
        truncation_notice(
            boundary.by_lines,
            content,
            boundary.total_lines,
            boundary.total_bytes,
        )
    };
    let mut notice = notice_for("");
    let mut content = String::new();
    for _pass in 0..4 {
        let content_bytes = DEFAULT_MAX_BYTES.saturating_sub(notice.len() + 1);
        let truncation = truncate_head(text, content_bytes, DEFAULT_MAX_LINES - 1);
        content = if truncation.first_line_exceeds_limit {
            truncate_utf8_prefix(text, content_bytes)
        } else {
            truncation.content
        };
        let next = notice_for(&content);
        let stable = next.len() == notice.len();
        notice = next;
        if stable {
            break;
        }
    }
    format!("{content}\n{notice}")
}

fn execute_command(
    command: &CuratedReadonlyCommand,
    cwd: &str,
    timeout_seconds: u64,
) -> Result<String, CuratedReadonlyCommandError> {
    let mut env: HashMap<String, String> = std::env::vars().collect();
    for (key, value) in [
        ("GH_PAGER", "cat"),
        ("GH_PROMPT_DISABLED", "1"),
        ("GIT_PAGER", "cat"),
        ("PAGER", "cat"),
    ] {
        env.insert(key.to_string(), value.to_string());
    }
    let result = run_process_with_tree_timeout(&ProcessTreeRunOptions {
        command: command.program.as_str().to_string(),
        args: command.args.clone(),
        cwd: PathBuf::from(cwd),
        env,
        max_buffer: MAX_BUFFER,
        timeout_ms: timeout_seconds * 1000,
        termination_grace_ms: None,
        termination_wait_ms: None,
        on_termination_report: None,
    });
    let stdout = result.stdout.trim();
    let stderr = result.stderr.trim();
    if result.exit_code != 0 || result.timed_out || result.signal.is_some() {
        let detail = if stderr.is_empty() {
            format!("exit code {}", result.exit_code)
        } else {
            stderr.to_string()
        };
        return Err(CuratedReadonlyCommandError(format!(
            "Read-only {} request failed: {detail}",
            command.program.as_str()
        )));
    }
    let joined = [stdout, stderr]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    Ok(if joined.is_empty() {
        "(no output)".to_string()
    } else {
        joined
    })
}
