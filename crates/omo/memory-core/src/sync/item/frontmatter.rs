//! Typed YAML frontmatter, resolved before CRDT classification.
use super::yaml::{Value, parse, source};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FmValue {
    Set { elements: Vec<String>, flow: String },
    Reg { value: String },
    Remove,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FmEntry {
    pub key: String,
    pub value: FmValue,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedItemText {
    pub fm: Option<Vec<FmEntry>>,
    pub body: String,
}

pub fn without_bom(text: &str) -> &str {
    text.strip_prefix('\u{feff}').unwrap_or(text)
}

pub fn split_header(text: &str) -> Option<(&str, &str)> {
    let open = if text.starts_with("---\r\n") {
        5
    } else if text.starts_with("---\n") {
        4
    } else {
        return None;
    };
    let end = text.get(open..)?.find("\n---")? + open;
    let nl = text[end + 4..].find('\n').map(|n| n + end + 4);
    if !text[end + 4..nl.unwrap_or(text.len())].trim().is_empty() {
        return None;
    }
    let header = text[open..end]
        .strip_suffix('\r')
        .unwrap_or(&text[open..end]);
    if header.trim().is_empty() {
        return None;
    }
    Some((header, nl.map_or("", |n| &text[n + 1..])))
}

pub fn split_item(raw: &str) -> ParsedItemText {
    let text = without_bom(raw);
    if let Some((header, body)) = split_header(text)
        && let Ok(node) = parse(header)
    {
        if node
            .pairs
            .iter()
            .any(|(key, _)| !matches!(key.value, Value::String(_)))
        {
            return ParsedItemText {
                fm: None,
                body: text.into(),
            };
        }
        if let Value::Map(pairs) = node.value {
            let fm = pairs
                .into_iter()
                .map(|(key, value)| FmEntry {
                    key,
                    value: classify(&value),
                })
                .collect();
            return ParsedItemText {
                fm: Some(fm),
                body: body.into(),
            };
        }
    }
    ParsedItemText {
        fm: None,
        body: text.into(),
    }
}

fn classify(value: &Value) -> FmValue {
    match value {
        Value::Null => FmValue::Remove,
        Value::Seq(items) => FmValue::Set {
            elements: items.iter().map(source).collect(),
            flow: source(value),
        },
        Value::String(_) | Value::Atom(_) | Value::Map(_) | Value::TypedMap(_) => FmValue::Reg {
            value: source(value),
        },
    }
}

pub fn sequence_elements(text: &str) -> Option<Vec<String>> {
    match parse(text).ok()?.value {
        Value::Seq(items) => Some(items.iter().map(source).collect()),
        _ => None,
    }
}

pub fn value_node(text: &str, verbatim: bool) -> Result<Value, super::ItemError> {
    if verbatim {
        let value = parse(text)?.value;
        Ok(match value {
            Value::Seq(_) | Value::Map(_) | Value::TypedMap(_) => Value::Atom(source(&value)),
            value => value,
        })
    } else {
        Ok(Value::Atom(super::yaml::double_quoted(text)))
    }
}

pub fn emit_frontmatter(entries: &[(String, Value)]) -> String {
    entries
        .iter()
        .map(|(key, value)| {
            let key = source(&Value::String(key.clone()));
            match value {
                Value::Seq(items) if !items.is_empty() => format!(
                    "{key}:\n{}",
                    items
                        .iter()
                        .map(|v| format!("  - {}", source(v)))
                        .collect::<Vec<_>>()
                        .join("\n")
                ),
                _ => format!("{key}: {}", source(value)),
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn render_entry(key: &str, value: &Value) -> String {
    emit_frontmatter(&[(key.into(), value.clone())])
}
