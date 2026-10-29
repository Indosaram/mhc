//! Event-level core-schema YAML with source spans and bounded alias resolution.
use super::ItemError;
use saphyr_parser::{Event, Parser, ScalarStyle as TScalarStyle, Span};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    Null,
    String(String),
    Atom(String),
    Seq(Vec<Value>),
    Map(Vec<(String, Value)>),
    TypedMap(Vec<(Value, Value)>),
}
#[derive(Clone, Debug)]
pub struct Node {
    pub value: Value,
    pub start: usize,
    pub end: usize,
    pub anchors: BTreeSet<usize>,
    pub aliases: BTreeSet<usize>,
    pub pairs: Vec<(Node, Node)>,
}
fn invalid() -> ItemError {
    ItemError::Yaml("invalid core YAML or alias expansion limit".into())
}

pub fn parse(text: &str) -> Result<Node, ItemError> {
    let mut events = Vec::new();
    for event in Parser::new_from_str(text) {
        events.push(event.map_err(|e| ItemError::Yaml(e.to_string()))?);
    }
    // Parser markers count Unicode scalars; disk splices index UTF-8 bytes.
    let offsets: Vec<_> = text.char_indices().map(|(at, _)| at).chain([text.len()]).collect();
    let mut at = 0;
    while matches!(
        events.get(at),
        Some((Event::StreamStart | Event::DocumentStart(_), _))
    ) {
        at += 1;
    }
    let mut anchors = BTreeMap::new();
    let mut budget = 64;
    let mut node = read_node(&events, &mut at, &mut anchors, &mut budget, 0)?;
    byte_spans(&mut node, &offsets)?;
    if events[at..]
        .iter()
        .any(|(e, _)| !matches!(e, Event::DocumentEnd | Event::StreamEnd))
    {
        return Err(invalid());
    }
    Ok(node)
}

fn byte_spans(node: &mut Node, offsets: &[usize]) -> Result<(), ItemError> {
    node.start = *offsets.get(node.start).ok_or_else(invalid)?;
    node.end = *offsets.get(node.end).ok_or_else(invalid)?;
    for (key, value) in &mut node.pairs {
        byte_spans(key, offsets)?;
        byte_spans(value, offsets)?;
    }
    Ok(())
}

fn read_node(
    events: &[(Event<'_>, Span)],
    at: &mut usize,
    book: &mut BTreeMap<usize, Node>,
    budget: &mut usize,
    depth: usize,
) -> Result<Node, ItemError> {
    if depth > 128 {
        return Err(invalid());
    }
    let (event, span) = events.get(*at).ok_or_else(invalid)?.clone();
    *at += 1;
    let mut node = Node {
        value: Value::Null,
        start: span.start.index(),
        end: span.end.index(),
        anchors: BTreeSet::new(),
        aliases: BTreeSet::new(),
        pairs: Vec::new(),
    };
    let anchor = match event {
        Event::Alias(id) => {
            let resolved = book.get(&id).ok_or_else(invalid)?;
            let cost = weight(&resolved.value);
            *budget = budget.checked_sub(cost).ok_or_else(invalid)?;
            node.value = resolved.value.clone();
            node.aliases.insert(id);
            0
        }
        Event::Scalar(text, style, id, tag) => {
            let explicit = tag.as_ref().map(|t| t.suffix.as_str());
            if tag
                .as_ref()
                .is_some_and(|t| t.handle != "tag:yaml.org,2002:" && t.handle != "!!")
            {
                return Err(invalid());
            }
            node.value = scalar(&text, style, explicit)?;
            id
        }
        Event::SequenceStart(id, tag) | Event::MappingStart(id, tag) => {
            let mapping = matches!(events[*at - 1].0, Event::MappingStart(..));
            if tag
                .as_ref()
                .is_some_and(|t| t.suffix != if mapping { "map" } else { "seq" })
            {
                return Err(invalid());
            }
            let mut values = Vec::new();
            loop {
                let (next, end) = events.get(*at).ok_or_else(invalid)?;
                if matches!(next, Event::SequenceEnd | Event::MappingEnd) {
                    node.end = end.end.index();
                    *at += 1;
                    break;
                }
                let child = read_node(events, at, book, budget, depth + 1)?;
                node.anchors.extend(&child.anchors);
                node.aliases.extend(&child.aliases);
                if mapping {
                    let value = read_node(events, at, book, budget, depth + 1)?;
                    node.anchors.extend(&value.anchors);
                    node.aliases.extend(&value.aliases);
                    node.pairs.push((child, value));
                } else {
                    values.push(child.value);
                }
            }
            node.value = if mapping {
                resolve_map(&node.pairs)?
            } else {
                Value::Seq(values)
            };
            id
        }
        _ => return Err(invalid()),
    };
    if anchor != 0 {
        node.anchors.insert(anchor);
        book.insert(anchor, node.clone());
    }
    Ok(node)
}

fn weight(value: &Value) -> usize {
    match value {
        Value::Seq(v) => 1 + v.iter().map(weight).sum::<usize>(),
        Value::Map(v) => 1 + v.iter().map(|(_, v)| weight(v)).sum::<usize>(),
        Value::TypedMap(v) => 1 + v.iter().map(|(k, v)| weight(k) + weight(v)).sum::<usize>(),
        _ => 1,
    }
}
fn resolve_map(pairs: &[(Node, Node)]) -> Result<Value, ItemError> {
    if pairs
        .iter()
        .any(|(k, _)| !matches!(k.value, Value::String(_)))
    {
        let mut out = Vec::new();
        for (key, value) in pairs {
            if out.iter().any(|(k, _)| k == &key.value) {
                return Err(invalid());
            }
            out.push((key.value.clone(), value.value.clone()));
        }
        return Ok(Value::TypedMap(out));
    }
    let mut out: Vec<(String, Value)> = Vec::new();
    let mut explicit = BTreeSet::new();
    for (key, value) in pairs {
        let key = match &key.value {
            Value::String(key) | Value::Atom(key) => key.clone(),
            Value::Null => "null".into(),
            Value::Seq(_) | Value::Map(_) | Value::TypedMap(_) => return Err(invalid()),
        };
        if key == "<<" {
            let maps = match &value.value {
                Value::Map(_) => vec![&value.value],
                Value::Seq(v) => v.iter().collect(),
                _ => return Err(invalid()),
            };
            for map in maps {
                let Value::Map(entries) = map else {
                    return Err(invalid());
                };
                for (k, v) in entries {
                    if !out.iter().any(|(key, _)| key == k) {
                        out.push((k.clone(), v.clone()));
                    }
                }
            }
        } else {
            if !explicit.insert(key.clone()) {
                return Err(invalid());
            }
            if let Some((_, old)) = out.iter_mut().find(|(k, _)| k == &key) {
                *old = value.value.clone();
            } else {
                out.push((key, value.value.clone()));
            }
        }
    }
    Ok(Value::Map(out))
}

fn scalar(text: &str, style: TScalarStyle, tag: Option<&str>) -> Result<Value, ItemError> {
    if tag == Some("str") || (tag.is_none() && style != TScalarStyle::Plain) {
        return Ok(Value::String(text.into()));
    }
    if tag.is_some_and(|t| !matches!(t, "str" | "null" | "bool" | "int" | "float")) {
        return Err(invalid());
    }
    let value = match text {
        "" | "~" | "null" | "Null" | "NULL" => Value::Null,
        "true" | "True" | "TRUE" => Value::Atom("true".into()),
        "false" | "False" | "FALSE" => Value::Atom("false".into()),
        ".inf" | ".Inf" | ".INF" | "+.inf" | "+.Inf" | "+.INF" => Value::Atom(".inf".into()),
        "-.inf" | "-.Inf" | "-.INF" => Value::Atom("-.inf".into()),
        ".nan" | ".NaN" | ".NAN" => Value::Atom(".nan".into()),
        _ => {
            let number = if let Some(hex) = text.strip_prefix("0x") {
                radix_number(hex, 16)
            } else if let Some(oct) = text.strip_prefix("0o") {
                radix_number(oct, 8)
            } else if text.chars().any(|c| c.is_ascii_digit()) {
                text.parse::<f64>().ok().map(number_source)
            } else {
                None
            };
            number.map_or_else(|| Value::String(text.into()), Value::Atom)
        }
    };
    if let Some(tag) = tag {
        let valid = match tag {
            "null" => value == Value::Null,
            "bool" => matches!(&value,Value::Atom(s) if matches!(s.as_str(),"true" | "false")),
            "int" => {
                matches!(&value,Value::Atom(s) if s.parse::<i64>().is_ok() || s.parse::<u64>().is_ok())
            }
            "float" => matches!(&value,Value::Atom(s) if !matches!(s.as_str(),"true" | "false")),
            _ => false,
        };
        if !valid {
            return Err(invalid());
        }
    }
    Ok(value)
}

fn radix_number(text: &str, radix: u32) -> Option<String> {
    if text.is_empty() {
        return None;
    }
    let mut number = 0.0;
    for c in text.chars() {
        number = number * f64::from(radix) + f64::from(c.to_digit(radix)?);
    }
    Some(number_source(number))
}
fn number_source(value: f64) -> String {
    if value.is_nan() {
        ".nan".into()
    } else if value == f64::INFINITY {
        ".inf".into()
    } else if value == f64::NEG_INFINITY {
        "-.inf".into()
    } else {
        super::super::canonical_json_serialize::canonical_number(value)
    }
}

// JSON permits literal NEL, LS and PS, while YAML treats them as line breaks even
// inside quotes. Escape those and YAML's forbidden C1 controls in stored sources.
pub fn double_quoted(text: &str) -> String {
    let json = serde_json::Value::String(text.into()).to_string();
    let mut out = String::with_capacity(json.len());
    for c in json.chars() {
        match c {
            '\u{7f}'..='\u{9f}' | '\u{2028}' | '\u{2029}' => {
                out.push_str(&format!("\\u{:04x}", u32::from(c)));
            }
            _ => out.push(c),
        }
    }
    out
}

pub fn source(value: &Value) -> String {
    match value {
        Value::TypedMap(v) => {
            if v.is_empty() {
                "{}".into()
            } else {
                format!(
                    "{{ {} }}",
                    v.iter()
                        .map(|(k, v)| format!("{}: {}", source(k), source(v)))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }
        }
        Value::Null => "null".into(),
        Value::Atom(s) => s.clone(),
        Value::String(s) => {
            let safe = !s.is_empty()
                && s.trim() == s
                && !s.contains([
                    '\n', '\r', '\t', '\u{2028}', '\u{2029}', ':', '#', '[', ']', '{', '}', ',', '"', '\'',
                ])
                && !s.starts_with(['-', '?', '!', '&', '*', '|', '>', '%', '@', '`'])
                && s.chars().all(|c| !c.is_control())
                && scalar(s, TScalarStyle::Plain, None)
                    .is_ok_and(|v| v == Value::String(s.clone()));
            if safe {
                s.clone()
            } else {
                double_quoted(s)
            }
        }
        Value::Seq(v) => {
            if v.is_empty() {
                "[]".into()
            } else {
                format!(
                    "[ {} ]",
                    v.iter().map(source).collect::<Vec<_>>().join(", ")
                )
            }
        }
        Value::Map(v) => {
            if v.is_empty() {
                "{}".into()
            } else {
                format!(
                    "{{ {} }}",
                    v.iter()
                        .map(|(k, v)| format!(
                            "{}: {}",
                            source(&Value::String(k.clone())),
                            source(v)
                        ))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }
        }
    }
}
