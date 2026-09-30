//! Port of senpi `packages/tui/src/autocomplete.ts`.
//!
//! Combined autocomplete provider handling `/` slash commands, `@` and bare-path file
//! suggestions (fd-backed fuzzy search plus directory listing), and `$` skill invocations.

use std::path::Path;
use std::process::Command;
use std::rc::Rc;

use crate::dollar_invocation_autocomplete::{
    find_dollar_skill_mentions, get_dollar_invocation_context, get_dollar_invocation_suggestions,
    known_skill_names,
};
use crate::slash_command_autocomplete::get_slash_command_suggestions;

const PATH_DELIMITERS: [char; 5] = [' ', '\t', '"', '\'', '='];

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AutocompleteItem {
    pub value: String,
    pub label: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutocompleteSuggestions {
    pub items: Vec<AutocompleteItem>,
    pub prefix: String,
}

/// Line-local character range, `end` exclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MentionRange {
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyCompletionResult {
    pub lines: Vec<String>,
    pub cursor_line: usize,
    pub cursor_col: usize,
}

pub type ArgumentCompletions = Rc<dyn Fn(&str) -> Option<Vec<AutocompleteItem>>>;

/// A slash command definition or a plain completion item; senpi's `SlashCommand | AutocompleteItem`.
#[derive(Clone)]
pub enum CommandSpec {
    Command {
        name: String,
        description: Option<String>,
        argument_hint: Option<String>,
        get_argument_completions: Option<ArgumentCompletions>,
    },
    Item(AutocompleteItem),
}

impl CommandSpec {
    pub fn command(name: &str) -> Self {
        CommandSpec::Command {
            name: name.to_string(),
            description: None,
            argument_hint: None,
            get_argument_completions: None,
        }
    }

    pub fn with_description(name: &str, description: &str) -> Self {
        CommandSpec::Command {
            name: name.to_string(),
            description: Some(description.to_string()),
            argument_hint: None,
            get_argument_completions: None,
        }
    }

    pub fn name(&self) -> &str {
        match self {
            CommandSpec::Command { name, .. } => name,
            CommandSpec::Item(item) => &item.value,
        }
    }

    pub fn description(&self) -> Option<&str> {
        match self {
            CommandSpec::Command { description, .. } => description.as_deref(),
            CommandSpec::Item(item) => item.description.as_deref(),
        }
    }

    pub fn argument_hint(&self) -> Option<&str> {
        match self {
            CommandSpec::Command { argument_hint, .. } => argument_hint.as_deref(),
            CommandSpec::Item(_) => None,
        }
    }

    pub fn argument_completions(&self) -> Option<&ArgumentCompletions> {
        match self {
            CommandSpec::Command {
                get_argument_completions,
                ..
            } => get_argument_completions.as_ref(),
            CommandSpec::Item(_) => None,
        }
    }
}

pub trait AutocompleteProvider {
    /// Characters that should naturally trigger this provider at token boundaries.
    fn trigger_characters(&self) -> Vec<String> {
        Vec::new()
    }

    fn get_suggestions(
        &mut self,
        lines: &[String],
        cursor_line: usize,
        cursor_col: usize,
        force: bool,
    ) -> Option<AutocompleteSuggestions>;

    fn apply_completion(
        &self,
        lines: &[String],
        cursor_line: usize,
        cursor_col: usize,
        item: &AutocompleteItem,
        prefix: &str,
    ) -> ApplyCompletionResult;

    fn should_trigger_file_completion(
        &self,
        _lines: &[String],
        _cursor_line: usize,
        _cursor_col: usize,
    ) -> bool {
        true
    }

    /// Ranges on one logical line that resolve to a known mention (for example a `$skill` token).
    fn get_mention_ranges(&self, _line: &str) -> Vec<MentionRange> {
        Vec::new()
    }
}

pub fn to_display_path(value: &str) -> String {
    value.replace('\\', "/")
}

fn escape_regex(value: &str) -> String {
    let special = ".*+?^${}()|[]\\";
    let mut out = String::new();
    for ch in value.chars() {
        if special.contains(ch) {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

fn build_fd_path_query(query: &str) -> String {
    let normalized = to_display_path(query);
    if !normalized.contains('/') {
        return normalized;
    }

    let has_trailing_separator = normalized.ends_with('/');
    let trimmed = normalized.trim_matches('/');
    if trimmed.is_empty() {
        return normalized;
    }

    let separator_pattern = "[\\\\/]";
    let segments: Vec<String> = trimmed
        .split('/')
        .filter(|segment| !segment.is_empty())
        .map(escape_regex)
        .collect();
    if segments.is_empty() {
        return normalized;
    }

    let mut pattern = segments.join(separator_pattern);
    if has_trailing_separator {
        pattern.push_str(separator_pattern);
    }
    pattern
}

fn find_last_delimiter(text: &str) -> Option<usize> {
    text.char_indices()
        .rev()
        .find(|(_, ch)| PATH_DELIMITERS.contains(ch))
        .map(|(index, _)| index)
}

fn find_unclosed_quote_start(text: &str) -> Option<usize> {
    let mut in_quotes = false;
    let mut quote_start = None;
    for (index, ch) in text.char_indices() {
        if ch == '"' {
            in_quotes = !in_quotes;
            if in_quotes {
                quote_start = Some(index);
            }
        }
    }
    if in_quotes {
        quote_start
    } else {
        None
    }
}

fn is_token_start(text: &str, index: usize) -> bool {
    if index == 0 {
        return true;
    }
    text[..index]
        .chars()
        .next_back()
        .is_some_and(|ch| PATH_DELIMITERS.contains(&ch))
}

fn extract_quoted_prefix(text: &str) -> Option<String> {
    let quote_start = find_unclosed_quote_start(text)?;

    if quote_start > 0 && text.as_bytes()[quote_start - 1] == b'@' {
        if !is_token_start(text, quote_start - 1) {
            return None;
        }
        return Some(text[quote_start - 1..].to_string());
    }

    if !is_token_start(text, quote_start) {
        return None;
    }

    Some(text[quote_start..].to_string())
}

struct ParsedPathPrefix {
    raw_prefix: String,
    is_at_prefix: bool,
    is_quoted_prefix: bool,
}

fn parse_path_prefix(prefix: &str) -> ParsedPathPrefix {
    if let Some(rest) = prefix.strip_prefix("@\"") {
        return ParsedPathPrefix {
            raw_prefix: rest.to_string(),
            is_at_prefix: true,
            is_quoted_prefix: true,
        };
    }
    if let Some(rest) = prefix.strip_prefix('"') {
        return ParsedPathPrefix {
            raw_prefix: rest.to_string(),
            is_at_prefix: false,
            is_quoted_prefix: true,
        };
    }
    if let Some(rest) = prefix.strip_prefix('@') {
        return ParsedPathPrefix {
            raw_prefix: rest.to_string(),
            is_at_prefix: true,
            is_quoted_prefix: false,
        };
    }
    ParsedPathPrefix {
        raw_prefix: prefix.to_string(),
        is_at_prefix: false,
        is_quoted_prefix: false,
    }
}

fn build_completion_value(path: &str, is_at_prefix: bool, is_quoted_prefix: bool) -> String {
    let needs_quotes = is_quoted_prefix || path.contains(' ');
    let prefix = if is_at_prefix { "@" } else { "" };

    if !needs_quotes {
        return format!("{prefix}{path}");
    }

    format!("{prefix}\"{path}\"")
}

// ---------- posix path helpers (Node `path` parity on Linux) ----------

fn normalize_posix(path: &str) -> String {
    let is_absolute = path.starts_with('/');
    let trailing = path.len() > 1 && path.ends_with('/');
    let mut out: Vec<&str> = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                if matches!(out.last(), Some(&last) if last != "..") {
                    out.pop();
                } else if !is_absolute {
                    out.push("..");
                }
            }
            other => out.push(other),
        }
    }
    let mut result = if is_absolute {
        format!("/{}", out.join("/"))
    } else {
        out.join("/")
    };
    if is_absolute && out.is_empty() {
        result = "/".to_string();
    }
    if trailing && !result.ends_with('/') {
        result.push('/');
    }
    if result.is_empty() {
        result = ".".to_string();
    }
    result
}

fn join_posix(parts: &[&str]) -> String {
    let joined = parts
        .iter()
        .filter(|part| !part.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join("/");
    if joined.is_empty() {
        ".".to_string()
    } else {
        normalize_posix(&joined)
    }
}

fn dirname_posix(path: &str) -> String {
    if path.is_empty() {
        return ".".to_string();
    }
    let bytes = path.as_bytes();
    let mut end = bytes.len();
    while end > 1 && bytes[end - 1] == b'/' {
        end -= 1;
    }
    if bytes[..end].iter().all(|&byte| byte == b'/') {
        return "/".to_string();
    }
    match path[..end].rfind('/') {
        None => ".".to_string(),
        Some(0) => "/".to_string(),
        Some(index) => {
            let mut cut = index;
            while cut > 0 && bytes[cut - 1] == b'/' {
                cut -= 1;
            }
            if cut == 0 {
                "/".to_string()
            } else {
                path[..cut].to_string()
            }
        }
    }
}

fn basename_posix(path: &str) -> String {
    if path.is_empty() {
        return String::new();
    }
    let bytes = path.as_bytes();
    let mut end = bytes.len();
    while end > 1 && bytes[end - 1] == b'/' {
        end -= 1;
    }
    let slice = &path[..end];
    match slice.rfind('/') {
        None => slice.to_string(),
        Some(index) => slice[index + 1..].to_string(),
    }
}

fn expand_home_path(path: &str, home: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/") {
        let expanded = join_posix(&[home, rest]);
        if path.ends_with('/') && !expanded.ends_with('/') {
            return format!("{expanded}/");
        }
        return expanded;
    }
    if path == "~" {
        return home.to_string();
    }
    path.to_string()
}

fn home_dir() -> String {
    std::env::var("HOME").unwrap_or_default()
}

fn locale_compare(left: &str, right: &str) -> std::cmp::Ordering {
    let left_lower = left.to_lowercase();
    let right_lower = right.to_lowercase();
    match left_lower.cmp(&right_lower) {
        std::cmp::Ordering::Equal => left.cmp(right),
        other => other,
    }
}

// ---------- fd walk ----------

fn walk_directory_with_fd(
    base_dir: &str,
    fd_path: &str,
    query: &str,
    max_results: usize,
    max_depth: Option<usize>,
) -> Vec<(String, bool)> {
    let mut args: Vec<String> = vec![
        "--base-directory".into(),
        base_dir.into(),
        "--max-results".into(),
        max_results.to_string(),
        "--type".into(),
        "f".into(),
        "--type".into(),
        "d".into(),
        "--follow".into(),
        "--hidden".into(),
        "--exclude".into(),
        ".git".into(),
        "--exclude".into(),
        ".git/*".into(),
        "--exclude".into(),
        ".git/**".into(),
    ];

    if let Some(depth) = max_depth {
        args.push("--max-depth".into());
        args.push(depth.to_string());
    }

    if to_display_path(query).contains('/') {
        args.push("--full-path".into());
    }

    if !query.is_empty() {
        args.push(build_fd_path_query(query));
    }

    let output = match Command::new(fd_path).args(&args).output() {
        Ok(output) => output,
        Err(_) => return Vec::new(),
    };
    if !output.status.success() {
        return Vec::new();
    }
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    if stdout.trim().is_empty() {
        return Vec::new();
    }

    let mut results = Vec::new();
    for line in stdout.trim().split('\n').filter(|line| !line.is_empty()) {
        let display_line = to_display_path(line);
        let has_trailing_separator = display_line.ends_with('/');
        let normalized_path = if has_trailing_separator {
            &display_line[..display_line.len() - 1]
        } else {
            display_line.as_str()
        };
        if normalized_path == ".git"
            || normalized_path.starts_with(".git/")
            || normalized_path.contains("/.git/")
        {
            continue;
        }
        results.push((display_line, has_trailing_separator));
    }
    results
}

pub struct CombinedAutocompleteProvider {
    commands: Vec<CommandSpec>,
    base_path: String,
    fd_path: Option<String>,
}

impl CombinedAutocompleteProvider {
    pub fn new(commands: Vec<CommandSpec>, base_path: &str, fd_path: Option<String>) -> Self {
        Self {
            commands,
            base_path: base_path.to_string(),
            fd_path,
        }
    }

    fn is_leading_known_skill_command_run(&self, text: &str) -> bool {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return false;
        }
        trimmed.split_whitespace().all(|token| {
            if !token.starts_with("/skill:") {
                return false;
            }
            let full = &token[1..];
            self.commands.iter().any(|command| command.name() == full)
        })
    }

    fn extract_at_prefix(&self, text: &str) -> Option<String> {
        if let Some(quoted) = extract_quoted_prefix(text) {
            if quoted.starts_with("@\"") {
                return Some(quoted);
            }
        }

        let token_start = match find_last_delimiter(text) {
            Some(index) => index + 1,
            None => 0,
        };

        if text[token_start..].starts_with('@') {
            return Some(text[token_start..].to_string());
        }

        None
    }

    fn extract_path_prefix(&self, text: &str, force_extract: bool) -> Option<String> {
        if let Some(quoted) = extract_quoted_prefix(text) {
            return Some(quoted);
        }

        let path_prefix = match find_last_delimiter(text) {
            Some(index) => text[index + 1..].to_string(),
            None => text.to_string(),
        };

        if force_extract {
            return Some(path_prefix);
        }

        if path_prefix.contains('/') || path_prefix.starts_with('.') || path_prefix.starts_with("~/") {
            return Some(path_prefix);
        }

        if path_prefix.is_empty() && text.ends_with(' ') {
            return Some(path_prefix);
        }

        None
    }

    fn resolve_scoped_fuzzy_query(&self, raw_query: &str) -> Option<(String, String, String)> {
        let normalized_query = to_display_path(raw_query);
        let slash_index = normalized_query.rfind('/')?;

        let display_base = normalized_query[..slash_index + 1].to_string();
        let query = normalized_query[slash_index + 1..].to_string();

        let base_dir = if display_base.starts_with("~/") {
            expand_home_path(&display_base, &home_dir())
        } else if display_base.starts_with('/') {
            display_base.clone()
        } else {
            join_posix(&[self.base_path.as_str(), display_base.as_str()])
        };

        if !Path::new(&base_dir).is_dir() {
            return None;
        }

        Some((base_dir, query, display_base))
    }

    fn scoped_path_for_display(display_base: &str, relative_path: &str) -> String {
        let normalized_relative_path = to_display_path(relative_path);
        if display_base == "/" {
            return format!("/{normalized_relative_path}");
        }
        format!("{}{}", to_display_path(display_base), normalized_relative_path)
    }

    fn get_file_suggestions(&self, prefix: &str) -> Vec<AutocompleteItem> {
        let parsed = parse_path_prefix(prefix);
        let raw_prefix = parsed.raw_prefix.clone();
        let expanded_prefix = if raw_prefix.starts_with('~') {
            expand_home_path(&raw_prefix, &home_dir())
        } else {
            raw_prefix.clone()
        };

        let is_root_prefix = raw_prefix.is_empty()
            || raw_prefix == "./"
            || raw_prefix == "../"
            || raw_prefix == "~"
            || raw_prefix == "~/"
            || raw_prefix == "/"
            || (parsed.is_at_prefix && raw_prefix.is_empty());

        let (search_dir, search_prefix) = if is_root_prefix {
            let search_dir = if raw_prefix.starts_with('~') || expanded_prefix.starts_with('/') {
                expanded_prefix.clone()
            } else {
                join_posix(&[self.base_path.as_str(), expanded_prefix.as_str()])
            };
            (search_dir, String::new())
        } else if raw_prefix.ends_with('/') {
            let search_dir = if raw_prefix.starts_with('~') || expanded_prefix.starts_with('/') {
                expanded_prefix.clone()
            } else {
                join_posix(&[self.base_path.as_str(), expanded_prefix.as_str()])
            };
            (search_dir, String::new())
        } else {
            let dir = dirname_posix(&expanded_prefix);
            let file = basename_posix(&expanded_prefix);
            let search_dir = if raw_prefix.starts_with('~') || expanded_prefix.starts_with('/') {
                dir
            } else {
                join_posix(&[self.base_path.as_str(), dir.as_str()])
            };
            (search_dir, file)
        };

        let entries = match std::fs::read_dir(&search_dir) {
            Ok(entries) => entries,
            Err(_) => return Vec::new(),
        };

        let mut suggestions: Vec<AutocompleteItem> = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.to_lowercase().starts_with(&search_prefix.to_lowercase()) {
                continue;
            }

            let mut is_directory = entry
                .file_type()
                .map(|file_type| file_type.is_dir())
                .unwrap_or(false);
            if !is_directory {
                if let Ok(metadata) = std::fs::metadata(entry.path()) {
                    is_directory = metadata.is_dir();
                }
            }

            let display_prefix = raw_prefix.as_str();
            let relative_path = if display_prefix.ends_with('/') {
                format!("{display_prefix}{name}")
            } else if display_prefix.contains('/') || display_prefix.contains('\\') {
                if let Some(home_relative_dir) = display_prefix.strip_prefix("~/") {
                    let dir = dirname_posix(home_relative_dir);
                    if dir == "." {
                        format!("~/{name}")
                    } else {
                        format!("~/{}", join_posix(&[dir.as_str(), name.as_str()]))
                    }
                } else if display_prefix.starts_with('/') {
                    let dir = dirname_posix(display_prefix);
                    if dir == "/" {
                        format!("/{name}")
                    } else {
                        format!("{dir}/{name}")
                    }
                } else {
                    let joined = join_posix(&[dirname_posix(display_prefix).as_str(), name.as_str()]);
                    if display_prefix.starts_with("./") && !joined.starts_with("./") {
                        format!("./{joined}")
                    } else {
                        joined
                    }
                }
            } else if display_prefix.starts_with('~') {
                format!("~/{name}")
            } else {
                name.clone()
            };

            let relative_path = to_display_path(&relative_path);
            let path_value = if is_directory {
                format!("{relative_path}/")
            } else {
                relative_path
            };
            let value = build_completion_value(&path_value, parsed.is_at_prefix, parsed.is_quoted_prefix);

            suggestions.push(AutocompleteItem {
                value,
                label: format!("{name}{}", if is_directory { "/" } else { "" }),
                description: None,
            });
        }

        suggestions.sort_by(|left, right| {
            let left_is_dir = left.value.ends_with('/');
            let right_is_dir = right.value.ends_with('/');
            if left_is_dir && !right_is_dir {
                return std::cmp::Ordering::Less;
            }
            if !left_is_dir && right_is_dir {
                return std::cmp::Ordering::Greater;
            }
            locale_compare(&left.label, &right.label)
        });

        suggestions
    }

    fn score_entry(file_path: &str, query: &str, is_directory: bool) -> i64 {
        let file_name = basename_posix(file_path);
        let lower_file_name = file_name.to_lowercase();
        let lower_query = query.to_lowercase();

        let mut score = if lower_file_name == lower_query {
            100
        } else if lower_file_name.starts_with(&lower_query) {
            80
        } else if lower_file_name.contains(&lower_query) {
            50
        } else if file_path.to_lowercase().contains(&lower_query) {
            30
        } else {
            0
        };

        if is_directory && score > 0 {
            score += 10;
        }

        score
    }

    fn get_base_dir_suggestions(&self, base_dir: &str, query: &str) -> Vec<(String, bool)> {
        let Some(fd_path) = &self.fd_path else {
            return Vec::new();
        };
        walk_directory_with_fd(base_dir, fd_path, query, 100, Some(1))
    }

    fn get_fuzzy_file_suggestions(&self, query: &str, is_quoted_prefix: bool) -> Vec<AutocompleteItem> {
        let Some(fd_path) = &self.fd_path else {
            return Vec::new();
        };

        let scoped_query = self.resolve_scoped_fuzzy_query(query);
        let (fd_base_dir, fd_query) = match &scoped_query {
            Some((base_dir, scoped, _)) => (base_dir.clone(), scoped.clone()),
            None => (self.base_path.clone(), query.to_string()),
        };

        let base_dir_entries = self.get_base_dir_suggestions(&fd_base_dir, &fd_query);
        let recursive_entries = walk_directory_with_fd(&fd_base_dir, fd_path, &fd_query, 100, None);

        let mut seen_paths: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut entries: Vec<(String, bool)> = Vec::new();
        for entry in base_dir_entries {
            seen_paths.insert(entry.0.clone());
            entries.push(entry);
        }
        for entry in recursive_entries {
            if seen_paths.insert(entry.0.clone()) {
                entries.push(entry);
            }
        }

        let mut scored: Vec<(i64, String, bool)> = entries
            .into_iter()
            .map(|(path, is_directory)| {
                let score = if fd_query.is_empty() {
                    1
                } else {
                    Self::score_entry(&path, &fd_query, is_directory)
                };
                (score, path, is_directory)
            })
            .filter(|(score, _, _)| *score > 0)
            .collect();

        scored.sort_by(|left, right| {
            let score_diff = right.0.cmp(&left.0);
            if score_diff != std::cmp::Ordering::Equal {
                return score_diff;
            }
            let left_depth = to_display_path(&left.1)
                .split('/')
                .filter(|part| !part.is_empty())
                .count();
            let right_depth = to_display_path(&right.1)
                .split('/')
                .filter(|part| !part.is_empty())
                .count();
            let depth_diff = left_depth.cmp(&right_depth);
            if depth_diff != std::cmp::Ordering::Equal {
                return depth_diff;
            }
            let length_diff = left.1.len().cmp(&right.1.len());
            if length_diff != std::cmp::Ordering::Equal {
                return length_diff;
            }
            locale_compare(&left.1, &right.1)
        });

        let mut suggestions = Vec::new();
        for (_, entry_path, is_directory) in scored.into_iter().take(20) {
            let path_without_slash = if is_directory {
                entry_path[..entry_path.len() - 1].to_string()
            } else {
                entry_path.clone()
            };
            let display_path = match &scoped_query {
                Some((_, _, display_base)) => Self::scoped_path_for_display(display_base, &path_without_slash),
                None => path_without_slash.clone(),
            };
            let entry_name = basename_posix(&path_without_slash);
            let completion_path = if is_directory {
                format!("{display_path}/")
            } else {
                display_path.clone()
            };
            let value = build_completion_value(&completion_path, true, is_quoted_prefix);

            suggestions.push(AutocompleteItem {
                value,
                label: format!("{entry_name}{}", if is_directory { "/" } else { "" }),
                description: Some(display_path),
            });
        }

        suggestions
    }
}

impl AutocompleteProvider for CombinedAutocompleteProvider {
    fn get_mention_ranges(&self, line: &str) -> Vec<MentionRange> {
        find_dollar_skill_mentions(line, &known_skill_names(&self.commands))
            .into_iter()
            .map(|mention| MentionRange {
                start: mention.start,
                end: mention.end,
            })
            .collect()
    }

    fn get_suggestions(
        &mut self,
        lines: &[String],
        cursor_line: usize,
        cursor_col: usize,
        force: bool,
    ) -> Option<AutocompleteSuggestions> {
        let current_line = lines.get(cursor_line).cloned().unwrap_or_default();
        let text_before_cursor = current_line
            .get(..cursor_col)
            .unwrap_or(&current_line)
            .to_string();

        if let Some(at_prefix) = self.extract_at_prefix(&text_before_cursor) {
            let parsed = parse_path_prefix(&at_prefix);
            let suggestions =
                self.get_fuzzy_file_suggestions(&parsed.raw_prefix, parsed.is_quoted_prefix);
            if suggestions.is_empty() {
                return None;
            }
            return Some(AutocompleteSuggestions {
                items: suggestions,
                prefix: at_prefix,
            });
        }

        if let Some(context) =
            get_dollar_invocation_context(&text_before_cursor, cursor_line, &self.commands)
        {
            let suggestions =
                get_dollar_invocation_suggestions(&self.commands, &context.query, context.skills_only);
            if suggestions.is_empty() {
                return None;
            }
            return Some(AutocompleteSuggestions {
                items: suggestions,
                prefix: context.prefix,
            });
        }

        if !force && text_before_cursor.starts_with('/') {
            if let Some(space_index) = text_before_cursor.find(' ') {
                let token_start = text_before_cursor
                    .char_indices()
                    .rev()
                    .find(|(_, ch)| !ch.is_whitespace())
                    .map(|(index, _)| index);
                let current_token = match token_start {
                    Some(index) => text_before_cursor[index..].to_string(),
                    None => String::new(),
                };
                if current_token.starts_with("/skill:") {
                    let before = match token_start {
                        Some(index) => &text_before_cursor[..index],
                        None => "",
                    };
                    if self.is_leading_known_skill_command_run(before) {
                        let skill_commands: Vec<CommandSpec> = self
                            .commands
                            .iter()
                            .filter(|command| command.name().starts_with("skill:"))
                            .cloned()
                            .collect();
                        let filtered = get_slash_command_suggestions(&skill_commands, &current_token[1..]);
                        if filtered.is_empty() {
                            return None;
                        }
                        return Some(AutocompleteSuggestions {
                            items: filtered,
                            prefix: current_token,
                        });
                    }
                }

                let command_name = &text_before_cursor[1..space_index];
                let argument_text = &text_before_cursor[space_index + 1..];

                let command = self
                    .commands
                    .iter()
                    .find(|command| command.name() == command_name);
                let Some(command) = command else {
                    return None;
                };
                let Some(get_argument_completions) = command.argument_completions() else {
                    return None;
                };
                let argument_suggestions = get_argument_completions(argument_text);
                let Some(argument_suggestions) = argument_suggestions else {
                    return None;
                };
                if argument_suggestions.is_empty() {
                    return None;
                }

                return Some(AutocompleteSuggestions {
                    items: argument_suggestions,
                    prefix: argument_text.to_string(),
                });
            }

            let prefix = &text_before_cursor[1..];
            let filtered = get_slash_command_suggestions(&self.commands, prefix);
            if filtered.is_empty() {
                return None;
            }
            return Some(AutocompleteSuggestions {
                items: filtered,
                prefix: text_before_cursor.clone(),
            });
        }

        let path_match = self.extract_path_prefix(&text_before_cursor, force)?;
        let suggestions = self.get_file_suggestions(&path_match);
        if suggestions.is_empty() {
            return None;
        }

        Some(AutocompleteSuggestions {
            items: suggestions,
            prefix: path_match,
        })
    }

    fn apply_completion(
        &self,
        lines: &[String],
        cursor_line: usize,
        cursor_col: usize,
        item: &AutocompleteItem,
        prefix: &str,
    ) -> ApplyCompletionResult {
        let current_line = lines.get(cursor_line).cloned().unwrap_or_default();
        let before_prefix = current_line
            .get(..cursor_col.saturating_sub(prefix.len()))
            .unwrap_or_default()
            .to_string();
        let after_cursor = current_line
            .get(cursor_col..)
            .unwrap_or_default()
            .to_string();
        let is_quoted_prefix = prefix.starts_with('"') || prefix.starts_with("@\"");
        let has_leading_quote_after_cursor = after_cursor.starts_with('"');
        let has_trailing_quote_in_item = item.value.ends_with('"');
        let adjusted_after_cursor =
            if is_quoted_prefix && has_trailing_quote_in_item && has_leading_quote_after_cursor {
                after_cursor[1..].to_string()
            } else {
                after_cursor.clone()
            };

        let mut new_lines = lines.to_vec();

        if prefix.starts_with('$') && (item.value.starts_with('/') || item.value.starts_with('$')) {
            let new_line = format!("{before_prefix}{} {adjusted_after_cursor}", item.value);
            new_lines[cursor_line] = new_line;
            return ApplyCompletionResult {
                lines: new_lines,
                cursor_line,
                cursor_col: before_prefix.len() + item.value.len() + 1,
            };
        }

        let is_slash_command = prefix.starts_with('/')
            && !prefix[1..].contains('/')
            && (before_prefix.trim().is_empty()
                || (prefix.starts_with("/skill:")
                    && self.is_leading_known_skill_command_run(&before_prefix)));
        if is_slash_command {
            let new_line = format!("{before_prefix}/{} {adjusted_after_cursor}", item.value);
            new_lines[cursor_line] = new_line;
            return ApplyCompletionResult {
                lines: new_lines,
                cursor_line,
                cursor_col: before_prefix.len() + item.value.len() + 2,
            };
        }

        if prefix.starts_with('@') {
            let is_directory = item.label.ends_with('/');
            let suffix = if is_directory { "" } else { " " };
            let new_line = format!("{before_prefix}{}{suffix}{adjusted_after_cursor}", item.value);
            new_lines[cursor_line] = new_line;

            let has_trailing_quote = item.value.ends_with('"');
            let cursor_offset = if is_directory && has_trailing_quote {
                item.value.len() - 1
            } else {
                item.value.len()
            };

            return ApplyCompletionResult {
                lines: new_lines,
                cursor_line,
                cursor_col: before_prefix.len() + cursor_offset + suffix.len(),
            };
        }

        let text_before_cursor = current_line
            .get(..cursor_col)
            .unwrap_or_default()
            .to_string();
        if text_before_cursor.contains('/') && text_before_cursor.contains(' ') {
            let new_line = format!("{before_prefix}{}{adjusted_after_cursor}", item.value);
            new_lines[cursor_line] = new_line;

            let is_directory = item.label.ends_with('/');
            let has_trailing_quote = item.value.ends_with('"');
            let cursor_offset = if is_directory && has_trailing_quote {
                item.value.len() - 1
            } else {
                item.value.len()
            };

            return ApplyCompletionResult {
                lines: new_lines,
                cursor_line,
                cursor_col: before_prefix.len() + cursor_offset,
            };
        }

        let new_line = format!("{before_prefix}{}{adjusted_after_cursor}", item.value);
        new_lines[cursor_line] = new_line;

        let is_directory = item.label.ends_with('/');
        let has_trailing_quote = item.value.ends_with('"');
        let cursor_offset = if is_directory && has_trailing_quote {
            item.value.len() - 1
        } else {
            item.value.len()
        };

        ApplyCompletionResult {
            lines: new_lines,
            cursor_line,
            cursor_col: before_prefix.len() + cursor_offset,
        }
    }

    fn should_trigger_file_completion(
        &self,
        lines: &[String],
        cursor_line: usize,
        cursor_col: usize,
    ) -> bool {
        let current_line = lines.get(cursor_line).cloned().unwrap_or_default();
        let text_before_cursor = current_line.get(..cursor_col).unwrap_or(&current_line);
        let trimmed = text_before_cursor.trim();
        if trimmed.starts_with('/') && !trimmed.contains(' ') {
            return false;
        }
        true
    }
}
