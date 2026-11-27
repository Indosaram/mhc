//! Port of `packages/coding-agent/src/core/tools/file-tool-host.ts`.

use crate::definition::ToolError;
use crate::file_mutation_queue::with_file_mutation_queue;
use std::collections::HashMap;
use std::future::Future;
use std::path::{Component, Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, OnceLock};
use tokio::sync::Mutex;
use unicode_normalization::UnicodeNormalization;

pub type ToolPathResolver = Arc<dyn Fn(&str, &Path) -> PathBuf + Send + Sync>;
pub type FileMutationQueueFn = Arc<
    dyn Fn(
            PathBuf,
            Box<dyn FnOnce() -> Pin<Box<dyn Future<Output = Result<(), ToolError>> + Send>> + Send>,
        ) -> Pin<Box<dyn Future<Output = Result<(), ToolError>> + Send>>
        + Send
        + Sync,
>;
pub type ToolPathCanonicalizer = Arc<
    dyn Fn(PathBuf) -> Pin<Box<dyn Future<Output = Result<PathBuf, ToolError>> + Send>>
        + Send
        + Sync,
>;

fn normalize_unicode_spaces(input: &str) -> String {
    input
        .chars()
        .map(|c| match c {
            '\u{00A0}' | '\u{2000}'..='\u{200A}' | '\u{202F}' | '\u{205F}' | '\u{3000}' => ' ',
            other => other,
        })
        .collect()
}

pub fn normalize_posix_path(path: &str) -> PathBuf {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." {
            parts.pop();
        } else {
            parts.push(part);
        }
    }
    PathBuf::from(format!("/{}", parts.join("/")))
}

/// POSIX resolution as `path.posix.resolve` does it, after clean-up (Unicode spaces, leading `@`,
/// `file://` URLs). `~` is not expanded because the host's home directory is unknown on a worker.
pub fn resolve_posix_tool_path(input: &str, cwd: &Path) -> PathBuf {
    let mut path = normalize_unicode_spaces(input);
    if let Some(stripped) = path.strip_prefix('@') {
        path = stripped.to_owned();
    }
    if let Some(rest) = path.strip_prefix("file://") {
        let pathname = rest.strip_prefix("localhost").unwrap_or(rest);
        path = percent_encoding::percent_decode_str(pathname)
            .decode_utf8_lossy()
            .into_owned();
    }
    let joined = if path.starts_with('/') {
        path
    } else {
        format!("{}/{path}", cwd.to_string_lossy().trim_end_matches('/'))
    };
    normalize_posix_path(&joined)
}

pub fn posix_dirname(path: &Path) -> PathBuf {
    let raw = path.to_string_lossy();
    let trimmed = if raw.len() > 1 {
        raw.trim_end_matches('/')
    } else {
        raw.as_ref()
    };
    match trimmed.rfind('/') {
        None => PathBuf::from("."),
        Some(0) => PathBuf::from("/"),
        Some(idx) => PathBuf::from(&trimmed[..idx]),
    }
}

pub fn normalize_lexical_join(base: &Path, rel: &Path) -> PathBuf {
    let joined = if rel.is_absolute() {
        rel.to_path_buf()
    } else {
        base.join(rel)
    };
    let mut out = PathBuf::new();
    for c in joined.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(out.components().next_back(), Some(Component::Normal(_))) {
                    out.pop();
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

static IN_PROCESS_QUEUES: OnceLock<std::sync::Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> =
    OnceLock::new();

pub async fn run_in_process_file_mutation_queue<T, F, Fut>(
    absolute_path: &Path,
    fn_call: F,
) -> Result<T, ToolError>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<T, ToolError>>,
{
    let lock = {
        let map = IN_PROCESS_QUEUES.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
        let mut guard = map.lock().unwrap_or_else(|e| e.into_inner());
        guard
            .entry(absolute_path.to_path_buf())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    };
    let _guard = lock.lock().await;
    fn_call().await
}

pub async fn run_local_file_mutation_queue<T, F, Fut>(
    absolute_path: &Path,
    fn_call: F,
) -> Result<T, ToolError>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<T, ToolError>>,
{
    with_file_mutation_queue(absolute_path, fn_call).await?
}

pub fn default_posix_path_resolver() -> ToolPathResolver {
    Arc::new(resolve_posix_tool_path)
}

pub fn lexical_canonical_path() -> ToolPathCanonicalizer {
    Arc::new(|path| Box::pin(async move { Ok(path) }))
}

const NARROW_NO_BREAK_SPACE: &str = "\u{202F}";

pub fn try_macos_screenshot_path(file_path: &str) -> String {
    let mut out = file_path.to_owned();
    for marker in ["AM", "PM", "am", "pm", "Am", "Pm", "aM", "pM"] {
        out = out.replace(
            &format!(" {marker}."),
            &format!("{NARROW_NO_BREAK_SPACE}{marker}."),
        );
    }
    out
}

pub fn try_nfd_variant(file_path: &str) -> String {
    file_path.nfd().collect()
}

pub fn try_curly_quote_variant(file_path: &str) -> String {
    file_path.replace('\'', "\u{2019}")
}

pub fn unquoted_path(file_path: &str) -> Option<String> {
    let trimmed = file_path.trim();
    let trimmed = trimmed.strip_prefix('@').unwrap_or(trimmed);
    let quote = trimmed.chars().next()?;
    if !matches!(quote, '\'' | '"')
        || !trimmed.ends_with(quote)
        || trimmed.len() <= quote.len_utf8() * 2
    {
        return None;
    }
    Some(trimmed[quote.len_utf8()..trimmed.len() - quote.len_utf8()].to_owned())
}

/// Resolve a read path, trying macOS AM/PM, NFD, curly quote, and quoted variants via `exists`.
pub async fn resolve_read_path_variants<F, Fut>(
    file_path: &str,
    cwd: &Path,
    resolve: &(dyn Fn(&str, &Path) -> PathBuf + Send + Sync),
    exists: F,
) -> PathBuf
where
    F: Fn(PathBuf) -> Fut + Send + Sync,
    Fut: Future<Output = bool> + Send,
{
    let mut current = file_path.to_owned();
    let resolved = loop {
        let candidate = resolve(&current, cwd);
        if exists(candidate.clone()).await {
            return candidate;
        }
        match unquoted_path(&current) {
            Some(unquoted) => current = unquoted,
            None => break candidate,
        }
    };

    let text = resolved.to_string_lossy();
    let am_pm = try_macos_screenshot_path(&text);
    if am_pm != text {
        let cand = PathBuf::from(&am_pm);
        if exists(cand.clone()).await {
            return cand;
        }
    }

    let nfd = try_nfd_variant(&text);
    if nfd != text {
        let cand = PathBuf::from(&nfd);
        if exists(cand.clone()).await {
            return cand;
        }
    }

    let curly = try_curly_quote_variant(&text);
    if curly != text {
        let cand = PathBuf::from(&curly);
        if exists(cand.clone()).await {
            return cand;
        }
    }

    let nfd_curly = try_curly_quote_variant(&nfd);
    if nfd_curly != text {
        let cand = PathBuf::from(&nfd_curly);
        if exists(cand.clone()).await {
            return cand;
        }
    }

    resolved
}
