//! Port of `packages/machine-ops/src/files/glob.ts`.

use crate::errors::{MachineOpError, OpErrorCode};
use crate::types::{AbortSignal, GlobEntry, GlobResult};
use regex::Regex;
use std::collections::VecDeque;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::AsyncBufReadExt;

/// Resolves the `fd` binary; `None` means `fd` is unavailable.
pub type FdResolver =
    Arc<dyn Fn() -> Pin<Box<dyn Future<Output = Option<PathBuf>> + Send>> + Send + Sync>;

const DEFAULT_LIMIT: usize = 1000;
const WALK_SKIP: &[&str] = &[".git", "node_modules"];

async fn path_exists(file_path: &Path) -> bool {
    tokio::fs::try_exists(file_path).await.unwrap_or(false)
}

async fn is_missing(dir: &Path) -> bool {
    match tokio::fs::metadata(dir).await {
        Ok(_) => false,
        Err(err) => {
            matches!(err.kind(), std::io::ErrorKind::NotFound)
                || crate::files::bounded_realpath::is_missing_path_error(&err)
        }
    }
}

fn is_executable(meta: &std::fs::Metadata) -> bool {
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        (meta.permissions().mode() & 0o111) != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Look `fd` (or Debian's `fdfind`) up on `PATH`.
async fn find_fd_on_path() -> Option<PathBuf> {
    let names: &[&str] = if cfg!(windows) {
        &["fd.exe"]
    } else {
        &["fd", "fdfind"]
    };
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        for name in names {
            let candidate = dir.join(name);
            if let Ok(meta) = tokio::fs::metadata(&candidate).await
                && is_executable(&meta)
            {
                return Some(candidate);
            }
        }
    }
    None
}

async fn run_fd_glob(
    fd_path: &Path,
    pattern: &str,
    search_path: &Path,
    limit: usize,
    signal: &AbortSignal,
) -> Result<GlobResult, MachineOpError> {
    let mut args: Vec<String> = vec![
        "--glob".to_owned(),
        "--color=never".to_owned(),
        "--hidden".to_owned(),
    ];

    let mut inside_git_repo = false;
    let mut current: Option<&Path> = Some(search_path);
    while let Some(dir) = current {
        if path_exists(&dir.join(".git")).await {
            inside_git_repo = true;
            break;
        }
        current = dir.parent();
    }
    if !inside_git_repo {
        args.push("--no-require-git".to_owned());
    }
    args.push("--max-results".to_owned());
    args.push(limit.to_string());

    let mut effective_pattern = pattern.to_owned();
    if pattern.contains('/') {
        args.push("--full-path".to_owned());
        if !pattern.starts_with('/') && !pattern.starts_with("**/") && pattern != "**" {
            effective_pattern = format!("**/{pattern}");
        }
        if cfg!(windows) {
            effective_pattern = effective_pattern.replace('/', r"[/\\]");
        }
    }
    args.push("--".to_owned());
    args.push(effective_pattern);
    args.push(search_path.to_string_lossy().into_owned());

    if signal.is_aborted() {
        return Err(MachineOpError::new(
            OpErrorCode::Aborted,
            "Operation aborted",
        ));
    }

    let mut cmd = tokio::process::Command::new(fd_path);
    cmd.args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    let mut child = cmd.spawn().map_err(|err| {
        MachineOpError::new(OpErrorCode::IoError, format!("Failed to run fd: {err}"))
    })?;

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let stdout_task = tokio::spawn(async move {
        let mut lines = Vec::new();
        if let Some(out) = stdout {
            let mut reader = tokio::io::BufReader::new(out).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                lines.push(line);
            }
        }
        lines
    });

    let stderr_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        if let Some(mut err_stream) = stderr {
            use tokio::io::AsyncReadExt;
            let _ = err_stream.read_to_end(&mut buf).await;
        }
        String::from_utf8_lossy(&buf).into_owned()
    });

    let status = tokio::select! {
        res = child.wait() => {
            res.map_err(|err| MachineOpError::new(OpErrorCode::IoError, format!("Failed to run fd: {err}")))?
        }
        () = signal.cancelled() => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            return Err(MachineOpError::new(OpErrorCode::Aborted, "Operation aborted"));
        }
    };

    if signal.is_aborted() {
        return Err(MachineOpError::new(
            OpErrorCode::Aborted,
            "Operation aborted",
        ));
    }

    let lines = stdout_task.await.unwrap_or_default();
    let stderr_text = stderr_task.await.unwrap_or_default();

    if !status.success() && lines.iter().all(|l| l.trim().is_empty()) {
        let message = if !stderr_text.trim().is_empty() {
            stderr_text.trim().to_owned()
        } else {
            format!("fd exited with code {}", status.code().unwrap_or(-1))
        };
        let code = if is_missing(search_path).await {
            OpErrorCode::NotFound
        } else {
            OpErrorCode::IoError
        };
        return Err(MachineOpError::new(code, message));
    }

    let entries: Vec<GlobEntry> = lines
        .into_iter()
        .map(|raw| raw.trim_end_matches('\r').trim().to_owned())
        .filter(|line| !line.is_empty())
        .map(|line| GlobEntry {
            path: PathBuf::from(line),
        })
        .collect();
    let truncated = entries.len() >= limit;
    Ok(GlobResult { entries, truncated })
}

/// Glob syntax (`**`, `*`, `?`, `{a,b}`, `[...]`) to a whole-string regular expression over `/`-separated paths.
pub fn glob_to_regex(glob: &str) -> Result<Regex, MachineOpError> {
    let chars: Vec<char> = glob.chars().collect();
    let mut source = String::new();
    let mut brace_depth: usize = 0;
    let mut index = 0;
    while index < chars.len() {
        let ch = chars[index];
        match ch {
            '*' => {
                if chars.get(index + 1) == Some(&'*') {
                    let slash_follows = chars.get(index + 2) == Some(&'/');
                    if slash_follows {
                        source.push_str("(?:.*/)?");
                        index += 3;
                    } else {
                        source.push_str(".*");
                        index += 2;
                    }
                } else {
                    source.push_str("[^/]*");
                    index += 1;
                }
            }
            '?' => {
                source.push_str("[^/]");
                index += 1;
            }
            '{' => {
                brace_depth += 1;
                source.push_str("(?:");
                index += 1;
            }
            '}' if brace_depth > 0 => {
                brace_depth -= 1;
                source.push(')');
                index += 1;
            }
            ',' if brace_depth > 0 => {
                source.push('|');
                index += 1;
            }
            '[' => {
                if let Some(rel_close) = chars[index + 1..].iter().position(|c| *c == ']') {
                    let close = index + 1 + rel_close;
                    let inner: String = chars[index + 1..close].iter().collect();
                    let transformed = if let Some(rest) = inner.strip_prefix('!') {
                        format!("^{rest}")
                    } else {
                        inner
                    }
                    .replace('\\', "\\\\");
                    source.push('[');
                    source.push_str(&transformed);
                    source.push(']');
                    index = close + 1;
                } else {
                    source.push_str("\\[");
                    index += 1;
                }
            }
            '.' | '+' | '^' | '$' | '(' | ')' | '|' | '\\' => {
                source.push('\\');
                source.push(ch);
                index += 1;
            }
            other => {
                source.push(other);
                index += 1;
            }
        }
    }
    Regex::new(&format!("^{source}$")).map_err(|err| {
        MachineOpError::new(
            OpErrorCode::InvalidPattern,
            format!("invalid glob pattern '{glob}': {err}"),
        )
    })
}

/// Pure-walker fallback when no `fd` exists: skips `.git` and `node_modules`, does not read `.gitignore`.
pub async fn walk_glob(
    pattern: &str,
    search_path: &Path,
    limit: usize,
    signal: &AbortSignal,
) -> Result<GlobResult, MachineOpError> {
    let trimmed = pattern.strip_prefix("./").unwrap_or(pattern);
    let matcher = glob_to_regex(trimmed)?;
    let match_basename = !pattern.contains('/');
    let mut entries = Vec::new();
    let mut pending = VecDeque::new();
    tokio::fs::metadata(search_path)
        .await
        .map_err(|err| MachineOpError::from_io(&err, Some("stat"), Some(search_path)))?;
    pending.push_back(search_path.to_path_buf());

    while let Some(dir) = pending.pop_front() {
        if entries.len() >= limit {
            break;
        }
        if signal.is_aborted() {
            return Err(MachineOpError::new(
                OpErrorCode::Aborted,
                "Operation aborted",
            ));
        }
        let Ok(mut rd) = tokio::fs::read_dir(&dir).await else {
            continue;
        };
        while let Ok(Some(dirent)) = rd.next_entry().await {
            let name = dirent.file_name();
            let name_str = name.to_string_lossy();
            if WALK_SKIP.contains(&name_str.as_ref()) {
                continue;
            }
            let full = dirent.path();
            let rel = full
                .strip_prefix(search_path)
                .unwrap_or(&full)
                .to_string_lossy()
                .replace(std::path::MAIN_SEPARATOR, "/");
            let candidate = if match_basename {
                name_str.as_ref()
            } else {
                rel.as_str()
            };
            if matcher.test(candidate) {
                entries.push(GlobEntry { path: full.clone() });
                if entries.len() >= limit {
                    break;
                }
            }
            if let Ok(ft) = dirent.file_type().await
                && ft.is_dir()
            {
                pending.push_back(full);
            }
        }
    }

    let truncated = entries.len() >= limit;
    Ok(GlobResult { entries, truncated })
}

trait RegexTestExt {
    fn test(&self, haystack: &str) -> bool;
}

impl RegexTestExt for Regex {
    fn test(&self, haystack: &str) -> bool {
        self.is_match(haystack)
    }
}

pub async fn local_glob(
    pattern: &str,
    search_path: &Path,
    limit: Option<usize>,
    signal: &AbortSignal,
    resolve_fd: Option<&FdResolver>,
) -> Result<GlobResult, MachineOpError> {
    let effective_limit = limit.unwrap_or(DEFAULT_LIMIT);
    if effective_limit == 0 {
        return Err(MachineOpError::new(
            OpErrorCode::InvalidArgument,
            "limit must be a positive integer",
        ));
    }
    if let Some(resolve_fd) = resolve_fd {
        let fd_path = resolve_fd().await;
        if signal.is_aborted() {
            return Err(MachineOpError::new(
                OpErrorCode::Aborted,
                "Operation aborted",
            ));
        }
        let Some(fd_path) = fd_path else {
            return Err(MachineOpError::new(
                OpErrorCode::IoError,
                "fd is not available and could not be downloaded",
            ));
        };
        return run_fd_glob(&fd_path, pattern, search_path, effective_limit, signal).await;
    }
    if let Some(fd_path) = find_fd_on_path().await {
        return run_fd_glob(&fd_path, pattern, search_path, effective_limit, signal).await;
    }
    walk_glob(pattern, search_path, effective_limit, signal).await
}
