//! The sidecar's `grep` tool (latest `kibitzer/tools/grep.ts`).
//!
//! A member-scoped grep over the workspace: the walk never follows symlinks, skips VCS/dependency
//! trees and binary or oversize files, and stops at the match cap. senpi withholds its builtin grep
//! from children, and a builtin would bypass the budget and the redaction anyway.
//!
//! The scan is bounded three more ways, because a rarely matching pattern over a large workspace used
//! to read every readable byte in it long after the wake that asked for it was gone: a file-count
//! budget, a byte budget over what was actually read, and a wall-clock budget (`caps.grep_scan*`),
//! plus the turn's abort signal, checked per directory while enumerating and before every file. The
//! first limit that trips ends the scan; the matches gathered so far are kept and `stopped` names the
//! limit it hit.
//!
//! Candidates come from `git ls-files` (tracked plus untracked-not-ignored) when the workspace root is
//! inside a work tree, so `.gitignore` is honored; any git failure falls back to the direct walk. An
//! explicitly named file is scanned as given - the path the model asked for is never second-guessed.
//!
//! The two upstream throws that are NOT caught must reach the caller: `realpath(workspaceRoot)` and
//! the walk's `readdir`. They are carried by [`KibitzerGrepError`], so `execute_grep` returns a
//! `Result<KibitzerToolResult, KibitzerGrepError>` and the CLI host surfaces the `Err` as the tool
//! call's failure. Every other failure is upstream's own structured rejection or a silent skip.
//!
//! Only the pure executor is ported here. The upstream file's host-bound halves - `KibitzerGrepParams`
//! (the typebox schema) and `createKibitzerGrepTool` (a `ToolDefinition` over a wake budget) - are
//! built by the CLI host (`crates/maho-cli/src/cli/kibitzer_tools.rs`), which also charges the
//! per-wake budget and forwards the turn's abort signal. Nothing in this module charges a budget.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use maho_tools::definition::AbortSignal;
use memory_core::git::exec::{GitExec, GitExecOptions, system_git_exec};
use serde_json::{Value, json};

use crate::kibitzer_tools_caps::KibitzerToolCaps;
use crate::kibitzer_tools_path_safety::{PathCheck, resolve_workspace_path};
use crate::kibitzer_tools_result::{
    KibitzerRejectionCode, KibitzerToolResult, bounded_text, ok_json, rejection,
};

/// The registered tool name (`grep`).
pub const KIBITZER_GREP_TOOL_NAME: &str = "grep";

/// One matching line of one scanned file.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct KibitzerGrepMatch {
    pub path: String,
    pub line: usize,
    pub text: String,
}

/// Why a scan ended early. Reported as `stopped` next to `truncated: true`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KibitzerGrepStopReason {
    Files,
    Bytes,
    Time,
    Matches,
    Aborted,
}

impl KibitzerGrepStopReason {
    /// The wire string reported as `stopped`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Files => "files",
            Self::Bytes => "bytes",
            Self::Time => "time",
            Self::Matches => "matches",
            Self::Aborted => "aborted",
        }
    }
}

/// A failure that upstream does not catch, so it rejects the whole call instead of becoming a tool
/// result. The CLI host maps it to the tool call's error; no rejection code is invented for it.
#[derive(Debug)]
pub enum KibitzerGrepError {
    /// `realpath(workspaceRoot)` failed (upstream: uncaught `await realpath`).
    RootUnresolved { path: String, message: String },
    /// A directory could not be listed (upstream: uncaught `readdir`).
    DirectoryUnreadable { path: String, message: String },
}

impl std::fmt::Display for KibitzerGrepError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RootUnresolved { path, message } | Self::DirectoryUnreadable { path, message } => {
                write!(formatter, "{message}: {path}")
            }
        }
    }
}

impl std::error::Error for KibitzerGrepError {}

const SKIPPED_DIRECTORIES: [&str; 2] = [".git", "node_modules"];
const MAX_FILE_BYTES: u64 = 1024 * 1024;
/// `git ls-files` reads the index once; it is not part of the scan budget, so it gets its own cap.
const GIT_LIST_TIMEOUT_MS: u64 = 5_000;

/// Everything the scan needs beyond its arguments.
struct ScanContext<'a> {
    caps: KibitzerToolCaps,
    now: &'a dyn Fn() -> i64,
    deadline: i64,
    signal: Option<&'a AbortSignal>,
}

/// The candidate list, plus the reason a bounded enumeration stopped early.
struct Candidates {
    files: Vec<String>,
    stopped: Option<KibitzerGrepStopReason>,
}

/// The bytes actually read from one file, and whether the match cap was reached while scanning it.
struct GrepFileOutcome {
    bytes: usize,
    capped: bool,
}

const SKIPPED_FILE: GrepFileOutcome = GrepFileOutcome { bytes: 0, capped: false };

/// Searches `workspace_root` (or one explicitly named file) for `pattern`, bounded by `caps`.
///
/// `now` is the injectable wall-clock source in milliseconds for the scan budget, and `git` is the
/// injectable runner for the gitignore-aware candidate list; an omitted runner defaults to the system
/// git (`input.git ?? createNodeGitExec`), never to the plain walk. `signal` is the turn's abort
/// signal - the one `maho_tools::definition::ToolCall` carries - which the CLI host forwards here;
/// `None` scans without cancellation.
///
/// `Err` is reserved for the two upstream throws this port must not swallow: an unresolvable
/// workspace root and an unlistable directory. Everything else is `Ok`, including the structured
/// rejections (`invalid_pattern`, `path_escape`, `not_found`, `missing_argument`).
pub fn execute_grep(
    workspace_root: &Path,
    caps: KibitzerToolCaps,
    now: &dyn Fn() -> i64,
    git: Option<&Arc<dyn GitExec>>,
    params: &Value,
    signal: Option<&AbortSignal>,
) -> Result<KibitzerToolResult, KibitzerGrepError> {
    let Some(pattern_text) = params.get("pattern").and_then(Value::as_str) else {
        return Ok(rejection(KibitzerRejectionCode::MissingArgument, "grep requires a pattern.", None));
    };
    let flags = if params.get("ignore_case").and_then(Value::as_bool) == Some(true) { "i" } else { "" };
    let pattern = match regress::Regex::from_unicode(pattern_text.encode_utf16().map(u32::from), flags) {
        Ok(pattern) => pattern,
        Err(error) => return Ok(rejection(KibitzerRejectionCode::InvalidPattern, &error.to_string(), None)),
    };
    let requested = params.get("path").and_then(Value::as_str);
    let input = requested.unwrap_or(".");
    let resolved = match resolve_workspace_path(workspace_root, input) {
        PathCheck::Ok { path } => path,
        PathCheck::Rejected { code, message } => return Ok(rejection(code, &message, requested)),
    };
    let Ok(info) = std::fs::metadata(&resolved) else {
        return Ok(rejection(KibitzerRejectionCode::NotFound, &format!("\"{input}\" does not exist."), requested));
    };
    // `await realpath(input.workspaceRoot)`: uncaught upstream, so a failure is a call error.
    let root = std::fs::canonicalize(workspace_root).map_err(|error| KibitzerGrepError::RootUnresolved {
        path: workspace_root.to_string_lossy().into_owned(),
        message: error.to_string(),
    })?;
    let context = ScanContext { caps, now, deadline: now() + caps.grep_scan_ms, signal };
    let candidates = if info.is_file() {
        Candidates { files: vec![resolved.clone()], stopped: None }
    } else {
        collect_candidates(&root, Path::new(&resolved), git, &context)?
    };
    let mut matches: Vec<KibitzerGrepMatch> = Vec::new();
    let mut stopped = candidates.stopped;
    let mut bytes: usize = 0;
    for file in &candidates.files {
        if is_aborted(signal) {
            stopped = stopped.or(Some(KibitzerGrepStopReason::Aborted));
            break;
        }
        if bytes >= caps.grep_scan_bytes {
            stopped = stopped.or(Some(KibitzerGrepStopReason::Bytes));
            break;
        }
        if now() >= context.deadline {
            stopped = stopped.or(Some(KibitzerGrepStopReason::Time));
            break;
        }
        let outcome = grep_file(Path::new(file), &display_path(&root, file), &pattern, &mut matches, caps);
        bytes += outcome.bytes;
        if outcome.capped {
            stopped = stopped.or(Some(KibitzerGrepStopReason::Matches));
            break;
        }
    }
    Ok(match stopped {
        None => ok_json(&json!({ "matches": matches, "truncated": false })),
        Some(reason) => ok_json(&json!({ "matches": matches, "truncated": true, "stopped": reason.as_str() })),
    })
}

/// The candidate list: the gitignore-aware one when git answers, else the direct walk.
fn collect_candidates(
    root: &Path,
    target: &Path,
    git: Option<&Arc<dyn GitExec>>,
    context: &ScanContext<'_>,
) -> Result<Candidates, KibitzerGrepError> {
    if is_aborted(context.signal) {
        return Ok(Candidates { files: Vec::new(), stopped: Some(KibitzerGrepStopReason::Aborted) });
    }
    // `input.git ?? createNodeGitExec()`: an omitted injectable runner defaults to the system git,
    // NOT to the plain walk. A SUPPLIED runner is always honored as given.
    let git = git.cloned().unwrap_or_else(system_git_exec);
    match git_candidates(root, target, &git, context) {
        Some(candidates) => Ok(candidates),
        None => walk(target, context),
    }
}

/// The gitignore-aware candidate list. `--cached --others --exclude-standard` is exactly "what git
/// would show you": tracked files plus untracked files that no ignore rule covers. Paths are printed
/// relative to the cwd, so running with `cwd: root` scopes the list to the workspace even when the
/// root is a subdirectory of the work tree. Returns `None` for anything that is not a clean success -
/// no repo, a git failure, a spawn error - so the caller falls back to the plain walk.
///
/// The runner is always present: the caller resolves an omitted injectable runner to the system git.
fn git_candidates(
    root: &Path,
    target: &Path,
    git: &Arc<dyn GitExec>,
    context: &ScanContext<'_>,
) -> Option<Candidates> {
    let options = GitExecOptions { cwd: root.to_path_buf(), timeout_ms: GIT_LIST_TIMEOUT_MS, ..GitExecOptions::default() };
    let toplevel = git.run(&argv(&["rev-parse", "--show-toplevel"]), &options).ok()?;
    if toplevel.code != 0 {
        return None;
    }
    let pathspec = relative_path(root, target);
    let pathspec = if pathspec.is_empty() { "." } else { pathspec.as_str() };
    let list_argv = argv(&["ls-files", "-z", "--cached", "--others", "--exclude-standard", "--", pathspec]);
    let listed = git.run(&list_argv, &options).ok()?;
    if listed.code != 0 {
        return None;
    }
    let mut seen: HashSet<String> = HashSet::new();
    let mut files: Vec<String> = Vec::new();
    for entry in listed.stdout.split('\0') {
        // Merge conflicts list a path once per stage, so the set also deduplicates.
        if entry.is_empty() || entry.split('/').any(|segment| SKIPPED_DIRECTORIES.contains(&segment)) {
            continue;
        }
        let full = root.join(entry).to_string_lossy().into_owned();
        if seen.insert(full.clone()) {
            files.push(full);
        }
    }
    Some(bounded(files, context.caps.grep_scan_files))
}

/// The direct walk: depth-first over a locale-sorted directory stack, never following a symlink and
/// skipping the VCS and dependency trees. A directory that cannot be listed propagates as
/// [`KibitzerGrepError::DirectoryUnreadable`], as upstream's uncaught `readdir` throws.
fn walk(directory: &Path, context: &ScanContext<'_>) -> Result<Candidates, KibitzerGrepError> {
    let mut files: Vec<String> = Vec::new();
    let mut pending: Vec<PathBuf> = vec![directory.to_path_buf()];
    let mut stopped: Option<KibitzerGrepStopReason> = None;
    while let Some(current) = pending.pop() {
        if is_aborted(context.signal) {
            stopped = Some(KibitzerGrepStopReason::Aborted);
            break;
        }
        if (context.now)() >= context.deadline {
            stopped = Some(KibitzerGrepStopReason::Time);
            break;
        }
        let mut entries: Vec<(String, std::fs::FileType)> = Vec::new();
        for entry in std::fs::read_dir(&current).map_err(|error| directory_unreadable(&current, error))? {
            let entry = entry.map_err(|error| directory_unreadable(&current, error))?;
            let file_type = entry.file_type().map_err(|error| directory_unreadable(&current, error))?;
            entries.push((entry.file_name().to_string_lossy().into_owned(), file_type));
        }
        let collator = locale_collator();
        entries.sort_by(|left, right| collator.compare(&left.0, &right.0));
        let mut overflowed = false;
        for (name, file_type) in entries {
            if file_type.is_symlink() {
                continue;
            }
            let full = current.join(&name);
            if file_type.is_dir() {
                if !SKIPPED_DIRECTORIES.contains(&name.as_str()) {
                    pending.push(full);
                }
            } else if file_type.is_file() {
                files.push(full.to_string_lossy().into_owned());
                // One past the budget is enough to know the tree does not fit; `bounded` cuts it back.
                if files.len() > context.caps.grep_scan_files {
                    overflowed = true;
                    break;
                }
            }
        }
        if overflowed {
            break;
        }
    }
    let result = bounded(files, context.caps.grep_scan_files);
    Ok(match stopped {
        None => result,
        Some(reason) => Candidates { files: result.files, stopped: Some(reason) },
    })
}

/// Locale-sorts the candidates and cuts them to `limit`, reporting `stopped: "files"` on overflow.
fn bounded(mut files: Vec<String>, limit: usize) -> Candidates {
    let collator = locale_collator();
    files.sort_by(|left, right| collator.compare(left, right));
    if files.len() <= limit {
        return Candidates { files, stopped: None };
    }
    files.truncate(limit);
    Candidates { files, stopped: Some(KibitzerGrepStopReason::Files) }
}

/// Scans one file: skips a non-regular, oversize, unreadable or binary file, counts the bytes it
/// actually read, and appends every matching line until the match cap is reached.
fn grep_file(
    file: &Path,
    display_path: &str,
    pattern: &regress::Regex,
    matches: &mut Vec<KibitzerGrepMatch>,
    caps: KibitzerToolCaps,
) -> GrepFileOutcome {
    // lstat, not stat: a git-listed path may be a symlink, a submodule gitlink, or an index entry
    // whose file is already gone, and the walk's candidates are regular files either way.
    let Ok(info) = std::fs::symlink_metadata(file) else {
        return SKIPPED_FILE;
    };
    if !info.is_file() || info.len() > MAX_FILE_BYTES {
        return SKIPPED_FILE;
    }
    let Ok(buffer) = std::fs::read(file) else {
        return SKIPPED_FILE;
    };
    let outcome = GrepFileOutcome { bytes: buffer.len(), capped: false };
    if buffer.iter().take(8192).any(|byte| *byte == 0) {
        return outcome;
    }
    let text = String::from_utf8_lossy(&buffer);
    for (index, line) in text.split('\n').enumerate() {
        if !matches_line(pattern, line) {
            continue;
        }
        if matches.len() >= caps.grep_matches {
            return GrepFileOutcome { bytes: outcome.bytes, capped: true };
        }
        matches.push(KibitzerGrepMatch {
            path: display_path.to_string(),
            line: index + 1,
            text: bounded_text(line, caps.grep_line_chars),
        });
    }
    outcome
}

/// JS `RegExp.prototype.test`: an unanchored search over UTF-16 code units, so an astral character
/// counts as the two units JavaScript sees and `\w`/`\d`/`.` keep their non-unicode meaning.
fn matches_line(pattern: &regress::Regex, line: &str) -> bool {
    let units: Vec<u16> = line.encode_utf16().collect();
    pattern.find_from_ucs2(&units, 0).next().is_some()
}

/// The port's established `localeCompare` reproduction: an ICU root collator, the same mechanism
/// `maho-ext-mcp`, `maho-ext-rules`, `maho-ext-tool-search`, `maho-ext-compaction`, `maho-ext-goal`
/// and `maho-interactive` use for JS `localeCompare`. `icu_collator = "=2.3.1"` must be added to this
/// crate by the sequential Cargo owner, exactly as those crates pin it.
fn locale_collator() -> icu_collator::CollatorBorrowed<'static> {
    icu_collator::Collator::try_new(Default::default(), Default::default())
        .expect("compiled collation data is available")
}

/// A directory that could not be listed, carrying the path the way upstream's message does.
fn directory_unreadable(path: &Path, error: std::io::Error) -> KibitzerGrepError {
    KibitzerGrepError::DirectoryUnreadable {
        path: path.to_string_lossy().into_owned(),
        message: error.to_string(),
    }
}

/// True when the turn's abort signal has fired.
fn is_aborted(signal: Option<&AbortSignal>) -> bool {
    signal.is_some_and(AbortSignal::is_aborted)
}

/// `path.relative(root, path)` for a path canonicalization already placed inside `root`.
fn relative_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .map_or_else(|_| path.to_string_lossy().into_owned(), |relative| relative.to_string_lossy().into_owned())
}

/// The path reported for a match: relative to the root, with `\` folded to `/` as upstream does.
fn display_path(root: &Path, file: &str) -> String {
    relative_path(root, Path::new(file)).replace('\\', "/")
}

/// `&[&str]` to the owned argv the git seam takes.
fn argv(args: &[&str]) -> Vec<String> {
    args.iter().map(|arg| (*arg).to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;
    use std::sync::Arc;

    use memory_core::git::exec::GitExecResult;
    use crate::kibitzer_tools_caps::DEFAULT_KIBITZER_TOOL_CAPS;

    /// The exact upstream fixture output (`grep.test.ts::SMALL_TREE_JSON`), byte for byte.
    const SMALL_TREE_JSON: &str = r#"{"matches":[{"path":"a.txt","line":1,"text":"alpha needle"},{"path":"src/app.ts","line":3,"text":"needle here"}],"truncated":false}"#;

    /// A temp root; the `TempDir` guard removes the tree when the test ends.
    fn temp_root() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    fn write(root: &Path, relative: &str, body: &str) {
        let full = root.join(relative);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).expect("create parent");
        }
        std::fs::write(full, body).expect("write");
    }

    fn write_bytes(root: &Path, relative: &str, body: &[u8]) {
        let full = root.join(relative);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).expect("create parent");
        }
        std::fs::write(full, body).expect("write");
    }

    /// Two files, one nested, both matching `needle` exactly once (`grep.test.ts::smallTree`).
    fn small_tree(root: &Path) {
        write(root, "a.txt", "alpha needle\nbeta\n");
        write(root, "src/app.ts", "export const token = 'value'\nexport const other = 1\nneedle here\n");
    }

    /// `count` sibling files named a.txt, b.txt, ... each holding one matching line.
    fn flat_tree(root: &Path, count: usize, line: &str) {
        for index in 0..count {
            let name = format!("{}.txt", char::from(b'a' + index as u8));
            write(root, &name, line);
        }
    }

    /// `git init` through the system git, the same runner the scan defaults to.
    fn git_init(root: &Path) {
        let result = system_git_exec().run_in(root, &["init"]).expect("git init spawn");
        assert_eq!(result.code, 0, "git init failed: {}", result.stderr);
    }

    /// The pure-executor call with a zero clock and no signal.
    fn run(root: &Path, caps: KibitzerToolCaps, git: Option<&Arc<dyn GitExec>>, params: &Value) -> KibitzerToolResult {
        let now = || 0i64;
        execute_grep(root, caps, &now, git, params, None).expect("execute_grep returns Ok")
    }

    fn json_of(result: &KibitzerToolResult) -> Value {
        serde_json::from_str(&result.text).expect("result text is JSON")
    }

    fn match_paths(result: &KibitzerToolResult) -> Vec<String> {
        json_of(result)
            .get("matches")
            .and_then(Value::as_array)
            .map(|matches| {
                matches
                    .iter()
                    .map(|entry| entry.get("path").and_then(Value::as_str).unwrap_or_default().to_string())
                    .collect()
            })
            .unwrap_or_default()
    }

    fn match_texts(result: &KibitzerToolResult) -> Vec<String> {
        json_of(result)
            .get("matches")
            .and_then(Value::as_array)
            .map(|matches| {
                matches
                    .iter()
                    .map(|entry| entry.get("text").and_then(Value::as_str).unwrap_or_default().to_string())
                    .collect()
            })
            .unwrap_or_default()
    }

    fn stopped(result: &KibitzerToolResult) -> Option<String> {
        json_of(result).get("stopped").and_then(Value::as_str).map(str::to_string)
    }

    fn is_truncated(result: &KibitzerToolResult) -> bool {
        json_of(result).get("truncated").and_then(Value::as_bool).unwrap_or(false)
    }

    /// A runner that always reports the given exit code with empty stdout, so the candidate list
    /// falls back to the plain walk (`grep.test.ts::failing`).
    struct FailingGit {
        code: i32,
    }

    impl GitExec for FailingGit {
        fn run(&self, _argv: &[String], _options: &GitExecOptions) -> io::Result<GitExecResult> {
            Ok(GitExecResult {
                code: self.code,
                stdout: String::new(),
                stderr: "fatal: not a git repository".to_string(),
            })
        }
    }

    // ---- the ten upstream `grep.test.ts` cases ------------------------------------------------

    #[test]
    fn given_a_tree_inside_every_budget_when_grep_runs_then_the_result_is_byte_identical_to_the_pre_change_output() {
        let root = temp_root();
        small_tree(root.path());
        // No runner: a non-repo root falls back to the walk, the pre-change path.
        let result = run(root.path(), DEFAULT_KIBITZER_TOOL_CAPS, None, &json!({ "pattern": "needle" }));
        assert_eq!(result.text, SMALL_TREE_JSON);
    }

    #[test]
    fn given_a_git_workspace_with_nothing_ignored_when_grep_runs_then_the_result_is_byte_identical_to_the_walk_output() {
        let root = temp_root();
        small_tree(root.path());
        git_init(root.path());
        // The runner is OMITTED, so this exercises the fixed default (`input.git ?? createNodeGitExec`).
        let result = run(root.path(), DEFAULT_KIBITZER_TOOL_CAPS, None, &json!({ "pattern": "needle" }));
        assert_eq!(result.text, SMALL_TREE_JSON);
    }

    #[test]
    fn given_more_files_than_the_file_budget_when_grep_runs_then_it_stops_at_files_and_keeps_the_matches_scanned_so_far() {
        let root = temp_root();
        flat_tree(root.path(), 3, "needle\n");
        let caps = KibitzerToolCaps { grep_scan_files: 2, ..DEFAULT_KIBITZER_TOOL_CAPS };
        let result = run(root.path(), caps, None, &json!({ "pattern": "needle" }));
        assert_eq!(match_paths(&result), vec!["a.txt", "b.txt"]);
        assert!(is_truncated(&result));
        assert_eq!(stopped(&result).as_deref(), Some("files"));
    }

    #[test]
    fn given_a_byte_budget_smaller_than_the_tree_when_grep_runs_then_it_stops_at_bytes_after_the_file_it_already_read() {
        let root = temp_root();
        flat_tree(root.path(), 3, "needle\n");
        let caps = KibitzerToolCaps { grep_scan_bytes: 1, ..DEFAULT_KIBITZER_TOOL_CAPS };
        let result = run(root.path(), caps, None, &json!({ "pattern": "needle" }));
        assert_eq!(match_paths(&result), vec!["a.txt"]);
        assert!(is_truncated(&result));
        assert_eq!(stopped(&result).as_deref(), Some("bytes"));
    }

    #[test]
    fn given_a_clock_past_the_time_budget_when_grep_runs_then_it_stops_at_time_with_the_matches_found_so_far() {
        // Every budget check reads the clock once; a 400ms step trips the 1000ms budget after the
        // first file (the deadline read plus the walk's per-directory read precede the loop).
        let root = temp_root();
        flat_tree(root.path(), 3, "needle\n");
        let elapsed = std::cell::Cell::new(0i64);
        let now = || {
            elapsed.set(elapsed.get() + 400);
            elapsed.get()
        };
        let caps = KibitzerToolCaps { grep_scan_ms: 1000, ..DEFAULT_KIBITZER_TOOL_CAPS };
        let result = execute_grep(root.path(), caps, &now, None, &json!({ "pattern": "needle" }), None).expect("ok");
        assert_eq!(match_paths(&result), vec!["a.txt"]);
        assert!(is_truncated(&result));
        assert_eq!(stopped(&result).as_deref(), Some("time"));
    }

    #[test]
    fn given_an_already_aborted_turn_when_grep_runs_then_it_stops_at_aborted_without_reading_a_file() {
        let root = temp_root();
        flat_tree(root.path(), 3, "needle\n");
        let signal = AbortSignal::default();
        signal.abort();
        let now = || 0i64;
        let result = execute_grep(root.path(), DEFAULT_KIBITZER_TOOL_CAPS, &now, None, &json!({ "pattern": "needle" }), Some(&signal)).expect("ok");
        assert_eq!(json_of(&result), json!({ "matches": [], "truncated": true, "stopped": "aborted" }));
    }

    #[test]
    fn given_a_turn_aborted_mid_scan_when_grep_runs_then_it_stops_at_aborted_and_keeps_the_matches_found_so_far() {
        // The clock is read once per file before it is scanned; aborting on the third read lands
        // between the first and the second file, preserving the executor's clock/check order.
        let root = temp_root();
        flat_tree(root.path(), 3, "needle\n");
        let signal = AbortSignal::default();
        let reads = std::cell::Cell::new(0usize);
        let now = || {
            reads.set(reads.get() + 1);
            if reads.get() >= 3 {
                signal.abort();
            }
            0i64
        };
        let result = execute_grep(root.path(), DEFAULT_KIBITZER_TOOL_CAPS, &now, None, &json!({ "pattern": "needle" }), Some(&signal)).expect("ok");
        assert_eq!(match_paths(&result), vec!["a.txt"]);
        assert!(is_truncated(&result));
        assert_eq!(stopped(&result).as_deref(), Some("aborted"));
    }

    #[test]
    fn given_more_matches_than_the_match_cap_when_grep_runs_then_it_stops_at_matches() {
        let root = temp_root();
        flat_tree(root.path(), 3, "needle\n");
        let caps = KibitzerToolCaps { grep_matches: 2, ..DEFAULT_KIBITZER_TOOL_CAPS };
        let result = run(root.path(), caps, None, &json!({ "pattern": "needle" }));
        assert_eq!(match_paths(&result), vec!["a.txt", "b.txt"]);
        assert!(is_truncated(&result));
        assert_eq!(stopped(&result).as_deref(), Some("matches"));
    }

    #[test]
    fn given_a_git_workspace_when_a_file_is_gitignored_then_grep_never_reports_a_match_inside_it() {
        let root = temp_root();
        flat_tree(root.path(), 2, "needle\n");
        write(root.path(), "ignored/secret.txt", "needle\n");
        write(root.path(), ".gitignore", "ignored/\n");
        git_init(root.path());
        let result = run(root.path(), DEFAULT_KIBITZER_TOOL_CAPS, None, &json!({ "pattern": "needle" }));
        assert_eq!(match_paths(&result), vec!["a.txt", "b.txt"]);
        assert!(!is_truncated(&result));
    }

    #[test]
    fn given_git_failing_on_the_workspace_when_grep_runs_then_it_falls_back_to_the_plain_walk() {
        let root = temp_root();
        flat_tree(root.path(), 2, "needle\n");
        write(root.path(), "ignored/secret.txt", "needle\n");
        write(root.path(), ".gitignore", "ignored/\n");
        git_init(root.path());
        let failing: Arc<dyn GitExec> = Arc::new(FailingGit { code: 128 });
        let result = run(root.path(), DEFAULT_KIBITZER_TOOL_CAPS, Some(&failing), &json!({ "pattern": "needle" }));
        assert_eq!(match_paths(&result), vec!["a.txt", "b.txt", "ignored/secret.txt"]);
        assert!(!is_truncated(&result));
    }

    // ---- recorded deterministic boundaries ------------------------------------------------------

    #[test]
    fn given_exactly_the_match_cap_when_grep_runs_then_the_result_is_not_truncated() {
        let root = temp_root();
        flat_tree(root.path(), 2, "needle\n");
        let caps = KibitzerToolCaps { grep_matches: 2, ..DEFAULT_KIBITZER_TOOL_CAPS };
        let result = run(root.path(), caps, None, &json!({ "pattern": "needle" }));
        assert_eq!(match_paths(&result), vec!["a.txt", "b.txt"]);
        assert!(!is_truncated(&result));
        assert_eq!(stopped(&result), None);
    }

    #[test]
    fn given_more_matching_lines_in_one_file_than_the_cap_when_grep_runs_then_it_reports_matches_at_the_next_line() {
        let root = temp_root();
        write(root.path(), "a.txt", "needle\nneedle\nneedle\n");
        let caps = KibitzerToolCaps { grep_matches: 2, ..DEFAULT_KIBITZER_TOOL_CAPS };
        let result = run(root.path(), caps, None, &json!({ "pattern": "needle" }));
        assert_eq!(match_paths(&result), vec!["a.txt", "a.txt"]);
        assert_eq!(stopped(&result).as_deref(), Some("matches"));
    }

    #[test]
    fn given_an_explicitly_named_gitignored_file_when_grep_runs_then_it_is_scanned_as_given() {
        let root = temp_root();
        flat_tree(root.path(), 2, "needle\n");
        write(root.path(), "ignored/secret.txt", "needle\n");
        write(root.path(), ".gitignore", "ignored/\n");
        git_init(root.path());
        // Directory enumeration omits the ignored path; naming it directly scans it as given.
        let result = run(root.path(), DEFAULT_KIBITZER_TOOL_CAPS, None, &json!({ "pattern": "needle", "path": "ignored/secret.txt" }));
        assert_eq!(match_paths(&result), vec!["ignored/secret.txt"]);
        assert!(!is_truncated(&result));
    }

    #[cfg(unix)]
    #[test]
    fn given_a_directory_symlink_when_grep_runs_then_the_walk_never_traverses_it() {
        let root = temp_root();
        write(root.path(), "real/inside.txt", "needle\n");
        std::os::unix::fs::symlink(root.path().join("real"), root.path().join("link")).expect("symlink");
        let result = run(root.path(), DEFAULT_KIBITZER_TOOL_CAPS, None, &json!({ "pattern": "needle" }));
        // Only the real directory's file is reported; the symlink is never entered.
        assert_eq!(match_paths(&result), vec!["real/inside.txt"]);
    }

    #[cfg(unix)]
    #[test]
    fn given_a_git_listed_file_symlink_when_grep_runs_then_it_is_skipped() {
        let root = temp_root();
        write(root.path(), "target.txt", "needle\n");
        std::os::unix::fs::symlink(root.path().join("target.txt"), root.path().join("link.txt")).expect("symlink");
        git_init(root.path());
        let result = run(root.path(), DEFAULT_KIBITZER_TOOL_CAPS, None, &json!({ "pattern": "needle" }));
        // The symlink is listed by git but dropped by lstat; only the real file matches.
        assert_eq!(match_paths(&result), vec!["target.txt"]);
    }

    #[test]
    fn given_a_binary_file_when_grep_runs_then_it_counts_the_read_bytes_but_yields_no_match() {
        let root = temp_root();
        // `a.txt` has a NUL in its first 8192 bytes, so it is binary: its full read length is
        // counted, but no line matches.
        let binary: Vec<u8> = {
            let mut bytes = b"needle\n".to_vec();
            bytes.push(0);
            bytes.extend_from_slice(b"needle\n");
            bytes
        };
        let cap = binary.len();
        write_bytes(root.path(), "a.txt", &binary);
        write(root.path(), "b.txt", "needle\n");
        // A byte budget equal to the binary read length stops at `bytes` before the next file.
        let caps = KibitzerToolCaps { grep_scan_bytes: cap, ..DEFAULT_KIBITZER_TOOL_CAPS };
        let result = run(root.path(), caps, None, &json!({ "pattern": "needle" }));
        assert!(match_paths(&result).is_empty(), "a NUL in the first 8192 bytes yields no match");
        assert_eq!(stopped(&result).as_deref(), Some("bytes"));
    }

    #[test]
    fn given_a_file_over_one_mib_when_grep_runs_then_it_is_skipped_without_contributing_read_bytes() {
        let root = temp_root();
        let oversize: Vec<u8> = {
            let mut bytes = b"needle\n".to_vec();
            bytes.resize(1024 * 1024 + 1, b'x');
            bytes
        };
        write_bytes(root.path(), "a.txt", &oversize);
        write(root.path(), "b.txt", "needle\n");
        // A one-byte budget: if the oversize file were read it would trip `bytes` before b.txt.
        let caps = KibitzerToolCaps { grep_scan_bytes: 1, ..DEFAULT_KIBITZER_TOOL_CAPS };
        let result = run(root.path(), caps, None, &json!({ "pattern": "needle" }));
        assert_eq!(match_paths(&result), vec!["b.txt"]);
        assert!(!is_truncated(&result));
    }

    #[test]
    fn given_a_candidate_enumeration_stop_when_the_scan_also_hits_a_later_limit_then_the_earlier_reason_survives() {
        let root = temp_root();
        flat_tree(root.path(), 3, "needle\n");
        let caps = KibitzerToolCaps { grep_scan_files: 2, grep_scan_bytes: 1, ..DEFAULT_KIBITZER_TOOL_CAPS };
        let result = run(root.path(), caps, None, &json!({ "pattern": "needle" }));
        assert_eq!(stopped(&result).as_deref(), Some("files"), "the enumeration stop reason is not overwritten by the later byte limit");
    }

    #[test]
    fn given_a_backreference_pattern_when_grep_runs_then_it_matches_like_javascript() {
        let root = temp_root();
        write(root.path(), "a.txt", "abab\nab\n");
        let result = run(root.path(), DEFAULT_KIBITZER_TOOL_CAPS, None, &json!({ "pattern": "(ab)\\1" }));
        assert_eq!(match_texts(&result), vec!["abab"]);
    }

    #[test]
    fn given_a_lookaround_pattern_when_grep_runs_then_it_matches_like_javascript() {
        let root = temp_root();
        write(root.path(), "a.txt", "price 100\nprice xyz\n");
        let result = run(root.path(), DEFAULT_KIBITZER_TOOL_CAPS, None, &json!({ "pattern": "price (?=\\d+)" }));
        assert_eq!(match_texts(&result), vec!["price 100"]);
    }

    #[test]
    fn given_an_astral_character_when_a_dot_matches_then_it_counts_utf16_code_units() {
        let root = temp_root();
        write(root.path(), "a.txt", "a\u{1f600}b\n");
        // No `u` flag: `.` is one UTF-16 code unit, so `a.b` cannot span the surrogate pair.
        let single = run(root.path(), DEFAULT_KIBITZER_TOOL_CAPS, None, &json!({ "pattern": "a.b" }));
        assert!(match_texts(&single).is_empty(), "a single dot cannot span the two UTF-16 units of an astral character");
        // Two dots cover the surrogate pair, matching JavaScript's code-unit semantics.
        let double = run(root.path(), DEFAULT_KIBITZER_TOOL_CAPS, None, &json!({ "pattern": "a..b" }));
        assert_eq!(match_texts(&double), vec!["a\u{1f600}b"]);
    }

    #[test]
    fn given_ignore_case_when_grep_runs_then_matching_folds_case() {
        let root = temp_root();
        write(root.path(), "a.txt", "NEEDLE\n");
        let insensitive = run(root.path(), DEFAULT_KIBITZER_TOOL_CAPS, None, &json!({ "pattern": "needle", "ignore_case": true }));
        assert_eq!(match_texts(&insensitive), vec!["NEEDLE"]);
        let sensitive = run(root.path(), DEFAULT_KIBITZER_TOOL_CAPS, None, &json!({ "pattern": "needle" }));
        assert!(match_texts(&sensitive).is_empty());
    }

    #[test]
    fn given_an_invalid_pattern_when_grep_runs_then_it_rejects_with_invalid_pattern() {
        let root = temp_root();
        write(root.path(), "a.txt", "needle\n");
        let result = run(root.path(), DEFAULT_KIBITZER_TOOL_CAPS, None, &json!({ "pattern": "(" }));
        assert!(result.is_error);
        let body = json_of(&result);
        assert_eq!(body.get("rejected").and_then(Value::as_str), Some("invalid_pattern"));
        assert!(body.get("path").is_none(), "the invalid-pattern rejection carries no path");
    }
}
