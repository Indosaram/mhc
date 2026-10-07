//! Tool-call payload to planner tokens (latest `recall-query-planner-tools.ts`).
//!
//! Harvests the eval keys the planner wants from a tool call: `command`/`code` produce path words
//! and the command name, `summary` produces its own tokens, and the path-ish keys produce their
//! file words. `ToolArgWindow` keeps the last eight pushes per session.

use std::collections::BTreeMap;
use std::path::Path;

use memory_core::sync::redact::contains_secret_like_material;
use serde_json::Value;

const TOOL_ARG_WINDOW: usize = 8;
const MAX_TOKENS: usize = 32;

fn is_usable(value: &str) -> bool {
    !value.is_empty() && value.chars().count() <= 120 && !contains_secret_like_material(value)
}

fn path_like(value: &str) -> bool {
    // `/` anywhere, or a short extension-like suffix `\.[a-z0-9]{1,6}` at end of the value.
    if value.contains('/') {
        return true;
    }
    let Some(dot) = value.rfind('.') else {
        return false;
    };
    let suffix = &value[dot + 1..];
    !suffix.is_empty()
        && suffix.len() <= 6
        && suffix.bytes().all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
}

fn basename(value: &str) -> &str {
    Path::new(value)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(value)
}

fn path_words(value: &str) -> Vec<String> {
    let name = basename(value);
    let mut words = vec![name.to_string()];
    words.extend(
        name.split(|ch| ch == '-' || ch == '_' || ch == '.')
            .filter(|word| word.chars().count() >= 3)
            .map(str::to_string),
    );
    words
}

fn shell_tokens(command: &str) -> Vec<String> {
    command
        .split_whitespace()
        .map(|token| token.trim_matches(|ch| ch == '\'' || ch == '"').to_string())
        .filter(|token| !token.is_empty())
        .collect()
}

fn path_chunks(token: &str) -> Vec<String> {
    if !token.contains('\'') && !token.contains('"') {
        return if path_like(token) { vec![token.to_string()] } else { vec![] };
    }
    token
        .split(|ch| ch == '\'' || ch == '"')
        .filter(|part| path_like(part) && !part.starts_with('-'))
        .map(str::to_string)
        .collect()
}

fn split_segments(command: &str) -> Vec<&str> {
    let mut segments = Vec::new();
    let mut start = 0usize;
    let bytes = command.as_bytes();
    let mut index = 0usize;
    while index < bytes.len() {
        let two = command.get(index..index + 2);
        if two == Some("||") || two == Some("&&") {
            segments.push(&command[start..index]);
            index += 2;
            start = index;
            continue;
        }
        if bytes[index] == b'|' || bytes[index] == b';' || bytes[index] == b'\n' {
            segments.push(&command[start..index]);
            index += 1;
            start = index;
            continue;
        }
        index += 1;
    }
    segments.push(&command[start..]);
    segments
}

fn command_texts(command: &str) -> Vec<String> {
    let mut result = Vec::new();
    for segment in split_segments(command) {
        if contains_secret_like_material(segment) {
            continue;
        }
        let tokens = shell_tokens(segment);
        let first = tokens.iter().find(|token| !token.starts_with('-'));
        for token in &tokens {
            if token.starts_with('-') {
                continue;
            }
            for word in path_chunks(token).into_iter().flat_map(|chunk| path_words(&chunk)) {
                if is_usable(&word) {
                    result.push(word);
                }
            }
        }
        if let Some(first) = first
            && is_usable(first)
        {
            result.push(first.clone());
        }
        if result.len() >= MAX_TOKENS {
            break;
        }
    }
    result.truncate(MAX_TOKENS);
    result
}

fn summary_texts(value: &str) -> Vec<String> {
    let mut result = Vec::new();
    if is_usable(value) {
        result.push(value.to_string());
    }
    for word in shell_tokens(value) {
        if path_like(&word) && !word.starts_with('-') {
            for part in path_words(&word) {
                if is_usable(&part) {
                    result.push(part);
                }
            }
        }
    }
    result
}

fn is_path_key(key: &str) -> bool {
    matches!(
        key,
        "path" | "filePath" | "file_path" | "target" | "file" | "pattern" | "query" | "command"
    )
}

fn is_array_key(key: &str) -> bool {
    matches!(key, "paths" | "files")
}

/// `toolArgTexts(toolName, input)`: the harvested eval keys, capped at [`MAX_TOKENS`].
pub fn tool_arg_texts(_tool_name: &str, input: &Value) -> Vec<String> {
    let mut result = Vec::new();
    let Some(object) = input.as_object() else {
        return result;
    };
    for (key, value) in object {
        if (key == "command" || key == "code") && let Some(text) = value.as_str() {
            result.extend(command_texts(text));
            continue;
        }
        if key == "summary" && let Some(text) = value.as_str() {
            result.extend(summary_texts(text));
            continue;
        }
        if is_path_key(key) && let Some(text) = value.as_str() {
            if !is_usable(text) {
                continue;
            }
            if path_like(text) {
                result.extend(path_words(text));
            } else {
                result.push(text.to_string());
            }
            continue;
        }
        if is_array_key(key) && let Some(items) = value.as_array() {
            for item in items {
                let Some(text) = item.as_str() else {
                    continue;
                };
                if !is_usable(text) {
                    continue;
                }
                if path_like(text) {
                    result.extend(path_words(text));
                } else {
                    result.push(text.to_string());
                }
            }
        }
    }
    result.truncate(MAX_TOKENS);
    result
}

/// Per-session rolling window of harvested tool-arg texts (last [`TOOL_ARG_WINDOW`] pushes).
#[derive(Default)]
pub struct ToolArgWindow {
    sessions: BTreeMap<String, Vec<Vec<String>>>,
}

impl ToolArgWindow {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, session_id: &str, texts: Vec<String>) {
        let pushes = self.sessions.entry(session_id.to_string()).or_default();
        pushes.push(texts);
        if pushes.len() > TOOL_ARG_WINDOW {
            let excess = pushes.len() - TOOL_ARG_WINDOW;
            pushes.drain(0..excess);
        }
    }

    pub fn texts(&self, session_id: &str) -> Vec<String> {
        self.sessions
            .get(session_id)
            .map(|pushes| pushes.iter().flatten().cloned().collect())
            .unwrap_or_default()
    }

    pub fn clear(&mut self, session_id: &str) {
        self.sessions.remove(session_id);
    }
}

#[cfg(test)]
#[path = "recall_planner_tools_tests.rs"]
mod tests;
