//! Port of senpi `packages/tui/src/dollar-invocation-autocomplete.ts`.

use std::collections::BTreeSet;
use std::sync::LazyLock;

use regex::Regex;

use crate::autocomplete::{AutocompleteItem, CommandSpec};
use crate::fuzzy::fuzzy_filter;

const SKILL_COMMAND_PREFIX: &str = "skill:";
static DOLLAR_QUERY_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\$([a-zA-Z0-9:_-]*)$").expect("valid dollar query regex"));
static DOLLAR_TOKEN_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\$([a-zA-Z][a-zA-Z0-9:_-]*)").expect("valid dollar token regex"));
static LAST_TOKEN_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\S+$").expect("valid token regex"));

const COMMON_SHELL_VARIABLES: &[&str] = &[
    "CI",
    "EDITOR",
    "HOME",
    "LANG",
    "LC_ALL",
    "NODE_ENV",
    "OLDPWD",
    "PATH",
    "PWD",
    "SHELL",
    "SHLVL",
    "TERM",
    "TMPDIR",
    "USER",
    "VISUAL",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DollarInvocationKind {
    Command,
    Skill,
}

#[derive(Debug, Clone)]
struct DollarInvocationItem {
    description: Option<String>,
    kind: DollarInvocationKind,
    label: String,
    search_text: String,
    value: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DollarInvocationContext {
    pub prefix: String,
    pub query: String,
    pub skills_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DollarSkillMention {
    pub start: usize,
    pub end: usize,
    pub name: String,
}

fn is_dollar_query_completable(query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    if query.starts_with('-') || query.starts_with('_') || query.starts_with(|c: char| c.is_ascii_digit()) {
        return false;
    }
    !COMMON_SHELL_VARIABLES.contains(&query)
}

fn skill_name(name: &str) -> Option<&str> {
    let value = name.strip_prefix(SKILL_COMMAND_PREFIX)?;
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

fn command_description(command: &CommandSpec) -> Option<String> {
    let hint = command.argument_hint().filter(|hint| !hint.is_empty());
    let description = command.description().unwrap_or("");
    match hint {
        Some(hint) => Some(if description.is_empty() {
            hint.to_string()
        } else {
            format!("{hint} — {description}")
        }),
        None => {
            if description.is_empty() {
                None
            } else {
                Some(description.to_string())
            }
        }
    }
}

fn is_whitespace_char(c: char) -> bool {
    c.is_whitespace()
}

/// Locate every `$name` / `$skill:name` token on one line that names a known skill.
///
/// Tokens must sit at a whitespace boundary, so `$HOME`, `$1`, `a$x`, and unknown names
/// stay plain. Offsets are line-local and `end` is exclusive.
pub fn find_dollar_skill_mentions(line: &str, known_skills: &BTreeSet<String>) -> Vec<DollarSkillMention> {
    if known_skills.is_empty() {
        return Vec::new();
    }
    let mut mentions = Vec::new();
    for captures in DOLLAR_TOKEN_PATTERN.captures_iter(line) {
        let whole = captures.get(0).expect("group 0");
        let token = captures.get(1).expect("group 1").as_str();
        let before_ok = line[..whole.start()]
            .chars()
            .next_back()
            .is_none_or(is_whitespace_char);
        let after_ok = line[whole.end()..]
            .chars()
            .next()
            .is_none_or(is_whitespace_char);
        if !before_ok || !after_ok {
            continue;
        }
        let name = token.strip_prefix(SKILL_COMMAND_PREFIX).unwrap_or(token);
        if !known_skills.contains(name) {
            continue;
        }
        mentions.push(DollarSkillMention {
            start: whole.start(),
            end: whole.end(),
            name: name.to_string(),
        });
    }
    mentions
}

/// Names of the skills the command list exposes as `skill:<name>` entries.
pub fn known_skill_names(commands: &[CommandSpec]) -> BTreeSet<String> {
    commands
        .iter()
        .filter_map(|command| skill_name(command.name()).map(str::to_string))
        .collect()
}

/// Describe the `$` token under the cursor when it should open the popup.
pub fn get_dollar_invocation_context(
    text_before_cursor: &str,
    _cursor_line: usize,
    commands: &[CommandSpec],
) -> Option<DollarInvocationContext> {
    let token_start = LAST_TOKEN_PATTERN.find(text_before_cursor).map(|m| m.start());
    let token = match token_start {
        Some(start) => &text_before_cursor[start..],
        None => "",
    };
    let captures = DOLLAR_QUERY_PATTERN.captures(token)?;
    let raw_query = captures.get(1).map(|m| m.as_str()).unwrap_or("");
    if !is_dollar_query_completable(raw_query) {
        return None;
    }
    let explicit_skill_namespace = raw_query.starts_with(SKILL_COMMAND_PREFIX);
    let query = if explicit_skill_namespace {
        &raw_query[SKILL_COMMAND_PREFIX.len()..]
    } else {
        raw_query
    };
    if known_skill_names(commands).contains(query) {
        return None;
    }
    let is_first_token = match token_start {
        Some(start) => text_before_cursor[..start].trim().is_empty(),
        None => true,
    };
    Some(DollarInvocationContext {
        prefix: format!("${raw_query}"),
        query: query.to_string(),
        skills_only: explicit_skill_namespace || !is_first_token,
    })
}

pub fn get_dollar_invocation_suggestions(
    commands: &[CommandSpec],
    query: &str,
    skills_only: bool,
) -> Vec<AutocompleteItem> {
    let mut items: Vec<DollarInvocationItem> = Vec::new();
    for command in commands {
        let name = command.name();
        if let Some(skill) = skill_name(name) {
            items.push(DollarInvocationItem {
                kind: DollarInvocationKind::Skill,
                value: format!("${skill}"),
                label: format!("${skill}"),
                search_text: skill.to_string(),
                description: command_description(command),
            });
            continue;
        }
        if skills_only {
            continue;
        }
        items.push(DollarInvocationItem {
            kind: DollarInvocationKind::Command,
            value: format!("/{name}"),
            label: format!("/{name}"),
            search_text: name.to_string(),
            description: command_description(command),
        });
    }

    let mut ranked: Vec<(usize, DollarInvocationItem)> = fuzzy_filter(&items, query, |item| item.search_text.clone())
        .into_iter()
        .enumerate()
        .collect();
    ranked.sort_by(|(left_index, left), (right_index, right)| {
        if left.kind != right.kind {
            return match left.kind {
                DollarInvocationKind::Command => std::cmp::Ordering::Less,
                DollarInvocationKind::Skill => std::cmp::Ordering::Greater,
            };
        }
        left_index.cmp(right_index)
    });
    ranked
        .into_iter()
        .map(|(_, item)| AutocompleteItem {
            value: item.value,
            label: item.label,
            description: item.description,
        })
        .collect()
}
