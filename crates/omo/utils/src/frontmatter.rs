//! YAML frontmatter parsing (JSON-schema YAML) plus a tolerant rule-file mode.

use std::sync::LazyLock;

use regex::Regex;
use serde_json::{Map, Value, json};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum FrontmatterMode {
    #[default]
    Default,
    Rule,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FrontmatterResult {
    pub data: Value,
    pub body: String,
    pub had_frontmatter: bool,
    pub parse_error: bool,
}

static FRONTMATTER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^---\r?\n((?s:.*?))\r?\n?---\r?\n((?s:.*))$")
        .unwrap_or_else(|error| panic!("{error}"))
});

fn empty() -> Value {
    Value::Object(Map::new())
}

pub fn parse_frontmatter(content: &str, mode: FrontmatterMode) -> FrontmatterResult {
    match mode {
        FrontmatterMode::Rule => return parse_rule_frontmatter(content),
        FrontmatterMode::Default => {}
    }
    let Some(captures) = FRONTMATTER.captures(content) else {
        return FrontmatterResult {
            data: empty(),
            body: content.to_string(),
            had_frontmatter: false,
            parse_error: false,
        };
    };
    let yaml = captures.get(1).map_or("", |m| m.as_str());
    let body = captures.get(2).map_or("", |m| m.as_str()).to_string();
    match parse_yaml(yaml) {
        Ok(data) => FrontmatterResult {
            data,
            body,
            had_frontmatter: true,
            parse_error: false,
        },
        Err(()) => FrontmatterResult {
            data: empty(),
            body,
            had_frontmatter: true,
            parse_error: true,
        },
    }
}

fn parse_yaml(yaml: &str) -> Result<Value, ()> {
    if yaml.trim().is_empty() {
        return Ok(empty());
    }
    let parsed: serde_yaml::Value = serde_yaml::from_str(yaml).map_err(|_| ())?;
    let value = serde_json::to_value(parsed).map_err(|_| ())?;
    Ok(if value.is_null() { empty() } else { value })
}

fn parse_rule_frontmatter(content: &str) -> FrontmatterResult {
    let normalized = content.strip_prefix('\u{feff}').unwrap_or(content);
    let opening = if normalized.starts_with("---\r\n") {
        5
    } else if normalized.starts_with("---\n") {
        4
    } else {
        0
    };
    let no_frontmatter = || FrontmatterResult {
        data: empty(),
        body: normalized.to_string(),
        had_frontmatter: false,
        parse_error: false,
    };
    if opening == 0 {
        return no_frontmatter();
    }
    let Some((start, body_start)) = find_closing(normalized, opening) else {
        return no_frontmatter();
    };
    FrontmatterResult {
        data: parse_rule_yaml(&normalized[opening..start]),
        body: normalized[body_start..].to_string(),
        had_frontmatter: true,
        parse_error: false,
    }
}

fn find_closing(content: &str, opening: usize) -> Option<(usize, usize)> {
    let mut line_start = opening;
    while line_start <= content.len() {
        let next_newline = content[line_start..]
            .find('\n')
            .map(|offset| line_start + offset);
        let line_end = next_newline.unwrap_or(content.len());
        if content[line_start..line_end].trim_end_matches('\r') == "---" {
            return Some((line_start, next_newline.map_or(content.len(), |n| n + 1)));
        }
        line_start = next_newline? + 1;
    }
    None
}

#[derive(Debug, Clone, PartialEq)]
enum Globs {
    One(String),
    Many(Vec<String>),
}

fn parse_rule_yaml(yaml: &str) -> Value {
    let text = yaml.replace("\r\n", "\n");
    let lines: Vec<&str> = text.split('\n').collect();
    let mut metadata = Map::new();
    let mut globs: Option<Globs> = None;
    let mut index = 0;
    while index < lines.len() {
        let line = strip_comment(lines[index]);
        let line = line.trim();
        let Some(colon) = line.find(':').filter(|_| !line.is_empty()) else {
            index += 1;
            continue;
        };
        let key = line[..colon].trim();
        let raw = line[colon + 1..].trim();
        match key {
            "description" => {
                metadata.insert("description".to_string(), json!(parse_string(raw)));
            }
            "alwaysApply" => {
                metadata.insert("alwaysApply".to_string(), json!(raw == "true"));
            }
            "globs" | "paths" | "applyTo" => {
                let (value, consumed) = parse_glob_value(raw, &lines, index);
                globs = Some(merge_globs(globs, value));
                index += consumed;
                continue;
            }
            _ => {}
        }
        index += 1;
    }
    match globs {
        Some(Globs::One(value)) => {
            metadata.insert("globs".to_string(), json!(value));
        }
        Some(Globs::Many(values)) => {
            metadata.insert("globs".to_string(), json!(values));
        }
        None => {}
    }
    Value::Object(metadata)
}

fn parse_glob_value(raw: &str, lines: &[&str], current: usize) -> (Globs, usize) {
    if raw.starts_with('[') {
        let values = raw
            .rfind(']')
            .map(|closing| {
                split_comma_separated(&raw[1..closing])
                    .iter()
                    .map(|v| parse_string(v))
                    .filter(|v| !v.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        return (Globs::Many(values), 1);
    }
    if raw.is_empty() {
        let mut values = Vec::new();
        let mut consumed = 1;
        for line in &lines[current + 1..] {
            let line = strip_comment(line);
            if line.trim().is_empty() {
                consumed += 1;
                continue;
            }
            let Some(item) = line
                .strip_prefix(|c: char| c.is_whitespace())
                .map(str::trim_start)
                .and_then(|rest| rest.strip_prefix('-'))
            else {
                break;
            };
            let value = parse_string(item);
            if !value.is_empty() {
                values.push(value);
            }
            consumed += 1;
        }
        return if values.is_empty() {
            (Globs::One(String::new()), 1)
        } else {
            (Globs::Many(values), consumed)
        };
    }
    let value = parse_string(raw);
    if value.contains(',') {
        return (
            Globs::Many(
                value
                    .split(',')
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                    .map(str::to_string)
                    .collect(),
            ),
            1,
        );
    }
    (Globs::One(value), 1)
}

fn merge_globs(existing: Option<Globs>, next: Globs) -> Globs {
    let next_is_empty = match &next {
        Globs::One(value) => value.is_empty(),
        Globs::Many(values) => values.is_empty(),
    };
    match (existing, next_is_empty) {
        (Some(existing), true) => existing,
        (None, true) => next,
        (None, false) => next,
        (Some(existing), false) => {
            let mut values = match existing {
                Globs::One(value) => vec![value],
                Globs::Many(values) => values,
            };
            match next {
                Globs::One(value) => values.push(value),
                Globs::Many(more) => values.extend(more),
            }
            Globs::Many(values)
        }
    }
}

fn split_comma_separated(value: &str) -> Vec<String> {
    let mut values = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for ch in value.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }
        if quote.is_some() && ch == '\\' {
            escaped = true;
            continue;
        }
        if ch == '"' || ch == '\'' {
            match quote {
                None => quote = Some(ch),
                Some(open) if open == ch => quote = None,
                Some(_) => {}
            }
            current.push(ch);
            continue;
        }
        if quote.is_none() && ch == ',' {
            values.push(current.trim().to_string());
            current.clear();
            continue;
        }
        current.push(ch);
    }
    values.push(current.trim().to_string());
    values
}

fn parse_string(value: &str) -> String {
    let trimmed = value.trim();
    let quoted = trimmed.len() >= 2
        && ((trimmed.starts_with('"') && trimmed.ends_with('"'))
            || (trimmed.starts_with('\'') && trimmed.ends_with('\'')));
    if quoted {
        trimmed[1..trimmed.len() - 1].to_string()
    } else {
        trimmed.to_string()
    }
}

fn strip_comment(line: &str) -> &str {
    let mut quote: Option<char> = None;
    for (index, ch) in line.char_indices() {
        if ch == '"' || ch == '\'' {
            match quote {
                None => quote = Some(ch),
                Some(open) if open == ch => quote = None,
                Some(_) => {}
            }
        }
        if quote.is_none() && ch == '#' {
            return &line[..index];
        }
    }
    line
}
