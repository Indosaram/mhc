//! Command-line matching primitives shared by sweep families. All matching is
//! token-aware: a path only counts when it appears as an executable token
//! (optionally preceded by a node/bun interpreter and runtime flags), never as
//! a data argument.

use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;

use crate::contains_path::lexical_normalize;

static NODE_EXECUTABLE: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"(?i)^node\d*(\.exe)?$").ok());
static BUN_EXECUTABLE: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"(?i)^bun(\.exe)?$").ok());

fn is_token_boundary(ch: char) -> bool {
    ch.is_whitespace() || ch == '"' || ch == '\''
}

/// Splits a command line on whitespace, honouring single and double quotes.
pub fn split_command_tokens(command: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut token_started = false;
    for ch in command.chars() {
        if let Some(open) = quote {
            if ch == open {
                quote = None;
            } else {
                current.push(ch);
            }
            continue;
        }
        if ch == '"' || ch == '\'' {
            quote = Some(ch);
            token_started = true;
            continue;
        }
        if ch.is_whitespace() {
            if token_started || !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
                token_started = false;
            }
            continue;
        }
        current.push(ch);
    }
    if token_started || !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

/// True when `expected_path` appears in `command` as a whole executable token.
pub fn has_executable_token(command: &str, expected_path: &str) -> bool {
    if expected_path.is_empty() {
        return false;
    }
    let mut search_from = 0;
    while let Some(offset) = command[search_from..].find(expected_path) {
        let path_index = search_from + offset;
        let token_start = find_token_start(command, path_index);
        let token_end = find_token_end(command, path_index + expected_path.len());
        if &command[token_start..token_end] == expected_path
            && token_looks_executable(command, token_start)
        {
            return true;
        }
        search_from = path_index + expected_path.len();
    }
    false
}

/// Matches an executable token that ends with `suffix` and lives under `root`
/// (e.g. suffix `/lsp-daemon/dist/cli.js` matches both the bundled
/// `<root>/components/lsp-daemon/dist/cli.js` and the packaged
/// `<root>/node_modules/@code-yeongyu/lsp-daemon/dist/cli.js`).
pub fn has_executable_token_under_root_with_suffix(
    command: &str,
    root: &str,
    suffix: &str,
) -> bool {
    if suffix.is_empty() {
        return false;
    }
    let root_prefix = format!("{root}/");
    let mut search_from = 0;
    while let Some(offset) = command[search_from..].find(suffix) {
        let suffix_index = search_from + offset;
        let token_start = find_token_start(command, suffix_index);
        let token_end = find_token_end(command, suffix_index + suffix.len());
        let token = &command[token_start..token_end];
        if token.ends_with(suffix)
            && token.starts_with(&root_prefix)
            && token_looks_executable(command, token_start)
        {
            return true;
        }
        search_from = suffix_index + suffix.len();
    }
    false
}

/// A token is executable when it starts the command, or when the nearest
/// non-flag token before it is a node/bun interpreter. Runtime flags between
/// the interpreter and the script are skipped (the real 1.4.1 daemon runs as
/// `<installDir>/node --liftoff-only <installDir>/lib/dist/bin/codegraph.js`).
pub fn token_looks_executable(command: &str, token_start: usize) -> bool {
    let mut prefix = command[..token_start].trim_end();
    if prefix.is_empty() {
        return true;
    }
    loop {
        let last_char_start = prefix.char_indices().last().map_or(0, |(index, _)| index);
        let previous_token_start = find_token_start(prefix, last_char_start);
        let previous_token = &prefix[previous_token_start..];
        if !previous_token.starts_with('-') {
            let executable_name = previous_token.rsplit('/').next().unwrap_or(previous_token);
            return NODE_EXECUTABLE
                .as_ref()
                .is_some_and(|re| re.is_match(executable_name))
                || BUN_EXECUTABLE
                    .as_ref()
                    .is_some_and(|re| re.is_match(executable_name));
        }
        prefix = prefix[..previous_token_start].trim_end();
        if prefix.is_empty() {
            return false;
        }
    }
}

/// Byte offset where the token containing the character before `index` starts.
pub fn find_token_start(command: &str, index: usize) -> usize {
    command[..index]
        .char_indices()
        .rev()
        .find(|(_, ch)| is_token_boundary(*ch))
        .map_or(0, |(offset, ch)| offset + ch.len_utf8())
}

/// Byte offset where the token that continues at `index` ends.
pub fn find_token_end(command: &str, index: usize) -> usize {
    command[index..]
        .char_indices()
        .find(|(_, ch)| is_token_boundary(*ch))
        .map_or(command.len(), |(offset, _)| index + offset)
}

/// Resolves, normalises and de-duplicates roots; longest root first.
pub fn normalize_roots(roots: &[String], platform: &str) -> Vec<String> {
    let mut normalized: Vec<String> = Vec::new();
    for root in roots {
        let trimmed = root.trim();
        if trimmed.is_empty() {
            continue;
        }
        let value =
            normalize_for_comparison(&resolve_path_for_platform(trimmed, platform), platform);
        if !normalized.contains(&value) {
            normalized.push(value);
        }
    }
    normalized.sort_by(|left, right| right.len().cmp(&left.len()).then_with(|| left.cmp(right)));
    normalized
}

/// Node `path.resolve` for the given platform flavour (`win32` or posix).
pub fn resolve_path_for_platform(value: &str, platform: &str) -> String {
    if platform == "win32" {
        return resolve_win32(value);
    }
    let path = Path::new(value);
    let absolute = if path.is_absolute() || value.starts_with('/') {
        path.to_path_buf()
    } else {
        std::env::current_dir().map_or_else(|_| path.to_path_buf(), |cwd| cwd.join(path))
    };
    lexical_normalize(&absolute).to_string_lossy().into_owned()
}

fn resolve_win32(value: &str) -> String {
    let unified = value.replace('/', "\\");
    let bytes = unified.as_bytes();
    let (prefix, rest) = if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        (unified[..2].to_uppercase(), &unified[2..])
    } else {
        (String::new(), unified.as_str())
    };
    let mut segments: Vec<&str> = Vec::new();
    for segment in rest.split('\\') {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            other => segments.push(other),
        }
    }
    format!("{prefix}\\{}", segments.join("\\"))
}

/// Forward slashes, no trailing slash, lower-case on Windows.
pub fn normalize_for_comparison(value: &str, platform: &str) -> String {
    let normalized = value.replace('\\', "/");
    let trimmed = normalized.trim_end_matches('/');
    if platform == "win32" {
        trimmed.to_lowercase()
    } else {
        trimmed.to_string()
    }
}
