use std::cmp::Ordering;

use serde_json::Map;
use serde_json::Number;
use serde_json::Value;
use serde_json::json;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizeError(pub String);

impl std::fmt::Display for NormalizeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for NormalizeError {}

fn invalid(label: &str) -> NormalizeError {
    NormalizeError(format!("Invalid {label}"))
}

fn object<'a>(
    value: Option<&'a Value>,
    label: &str,
) -> Result<&'a Map<String, Value>, NormalizeError> {
    value
        .and_then(Value::as_object)
        .ok_or_else(|| invalid(label))
}

fn number<'a>(value: Option<&'a Value>, label: &str) -> Result<&'a Number, NormalizeError> {
    match value {
        Some(Value::Number(number)) => Ok(number),
        _ => Err(invalid(label)),
    }
}

fn plus_one(number: &Number) -> Value {
    if let Some(value) = number.as_i64() {
        return Value::from(value + 1);
    }
    Value::from(number.as_f64().unwrap_or(0.0) + 1.0)
}

fn point(raw: Option<&Value>, byte_offset: &Number) -> Result<Value, NormalizeError> {
    let value = object(raw, "range point")?;
    Ok(json!({
        "line": plus_one(number(value.get("line"), "line")?),
        "column": number(value.get("column"), "column")?,
        "byteOffset": byte_offset,
    }))
}

fn is_windows_path(value: &str) -> bool {
    let bytes = value.as_bytes();
    let drive = bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/');
    drive || value.starts_with("\\\\")
}

fn normalize_segments<'a>(segments: impl Iterator<Item = &'a str>, into: &mut Vec<String>) {
    for segment in segments {
        match segment {
            "" | "." => {}
            ".." => {
                into.pop();
            }
            other => into.push(other.to_owned()),
        }
    }
}

fn posix_resolve(root: &str, file: &str) -> String {
    let mut segments = Vec::new();
    if !file.starts_with('/') {
        if !root.starts_with('/') {
            let cwd = std::env::current_dir()
                .map(|path| path.to_string_lossy().into_owned())
                .unwrap_or_else(|_| "/".to_owned());
            normalize_segments(cwd.split('/'), &mut segments);
        }
        normalize_segments(root.split('/'), &mut segments);
    }
    normalize_segments(file.split('/'), &mut segments);
    format!("/{}", segments.join("/"))
}

fn relative_inside(
    root: &[String],
    absolute: &[String],
    eq: impl Fn(&str, &str) -> bool,
) -> Option<String> {
    if absolute.len() < root.len() || !root.iter().zip(absolute).all(|(a, b)| eq(a, b)) {
        return None;
    }
    let relative = absolute[root.len()..].join("/");
    Some(if relative.is_empty() {
        ".".to_owned()
    } else {
        relative
    })
}

fn windows_split(value: &str) -> (String, Vec<String>) {
    let value = value.replace('/', "\\");
    let (prefix, rest) = if value.len() >= 2 && value.as_bytes()[1] == b':' {
        (value[..2].to_ascii_uppercase(), value[2..].to_owned())
    } else if let Some(unc) = value.strip_prefix("\\\\") {
        let mut parts = unc.splitn(3, '\\');
        let server = parts.next().unwrap_or_default();
        let share = parts.next().unwrap_or_default();
        (
            format!("\\\\{server}\\{share}"),
            parts.next().unwrap_or_default().to_owned(),
        )
    } else {
        (String::new(), value)
    };
    let mut segments = Vec::new();
    normalize_segments(rest.split('\\'), &mut segments);
    (prefix, segments)
}

fn windows_stable_path(file: &str, workdir: &str) -> String {
    let (root_prefix, root) = windows_split(workdir);
    let (file_prefix, file_segments) = if is_windows_path(file) {
        windows_split(file)
    } else {
        let (_, relative) = windows_split(file);
        let mut joined = root.clone();
        normalize_segments(relative.iter().map(String::as_str), &mut joined);
        (root_prefix.clone(), joined)
    };
    if file_prefix.eq_ignore_ascii_case(&root_prefix)
        && let Some(relative) =
            relative_inside(&root, &file_segments, |a, b| a.eq_ignore_ascii_case(b))
    {
        return relative;
    }
    format!("{file_prefix}/{}", file_segments.join("/"))
}

fn stable_path(file: &str, workdir: &str) -> String {
    if is_windows_path(file) || is_windows_path(workdir) {
        return windows_stable_path(file, workdir);
    }
    let root = posix_resolve(workdir, "");
    let absolute = posix_resolve(&root, file);
    let split = |path: &str| {
        path.split('/')
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    relative_inside(&split(&root), &split(&absolute), |a, b| a == b).unwrap_or(absolute)
}

fn node_text(value: &Value) -> Result<String, NormalizeError> {
    if let Value::String(text) = value {
        return Ok(text.clone());
    }
    let node = object(Some(value), "metavariable node")?;
    Ok(node
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned())
}

fn node_byte_range(value: &Value) -> Result<Option<(f64, f64)>, NormalizeError> {
    let Value::Object(node) = value else {
        return Ok(None);
    };
    let range = object(node.get("range"), "metavariable range")?;
    let bytes = object(range.get("byteOffset"), "metavariable byte range")?;
    Ok(
        match (
            bytes.get("start").and_then(Value::as_f64),
            bytes.get("end").and_then(Value::as_f64),
        ) {
            (Some(start), Some(end)) => Some((start, end)),
            _ => None,
        },
    )
}

fn slice_bytes(text: &str, start: f64, end: f64) -> Option<String> {
    let len = text.len() as f64;
    if !(start >= 0.0 && end >= start && end <= len) || start.fract() != 0.0 || end.fract() != 0.0 {
        return None;
    }
    let bytes = &text.as_bytes()[start as usize..end as usize];
    std::str::from_utf8(bytes).ok().map(str::to_owned)
}

fn normalize_metavariables(
    raw: Option<&Value>,
    text: &str,
    match_start: f64,
) -> Result<Value, NormalizeError> {
    let empty = Map::new();
    let meta = match raw {
        None => &empty,
        Some(value) => object(Some(value), "metaVariables")?,
    };
    let singles = match meta.get("single") {
        None => &empty,
        Some(value) => object(Some(value), "single metavariables")?,
    };
    let multis = match meta.get("multi") {
        None => &empty,
        Some(value) => object(Some(value), "multi metavariables")?,
    };
    let mut single = Map::new();
    for (name, value) in singles {
        single.insert(name.clone(), Value::String(node_text(value)?));
    }
    let mut multi = Map::new();
    for (name, value) in multis {
        let nodes = match value {
            Value::Array(nodes) if !nodes.is_empty() => nodes,
            _ => {
                multi.insert(name.clone(), Value::from(""));
                continue;
            }
        };
        let first = node_byte_range(&nodes[0])?;
        let last = node_byte_range(&nodes[nodes.len() - 1])?;
        let start = first.map_or(match_start, |range| range.0) - match_start;
        let end = last.map_or(match_start, |range| range.1) - match_start;
        let collapsed = match slice_bytes(text, start, end) {
            Some(collapsed) => collapsed,
            None => nodes
                .iter()
                .map(node_text)
                .collect::<Result<Vec<_>, _>>()?
                .concat(),
        };
        multi.insert(name.clone(), Value::String(collapsed));
    }
    Ok(json!({ "single": single, "multi": multi }))
}

const CONSUMED: [&str; 8] = [
    "text",
    "range",
    "file",
    "lines",
    "language",
    "metaVariables",
    "charCount",
    "transformed",
];

pub fn normalize_match(
    raw: &Map<String, Value>,
    workdir: &str,
) -> Result<Map<String, Value>, NormalizeError> {
    let text = raw.get("text").and_then(Value::as_str).unwrap_or_default();
    let file = raw.get("file").and_then(Value::as_str).unwrap_or_default();
    let range = object(raw.get("range"), "range")?;
    let bytes = object(range.get("byteOffset"), "byte range")?;
    let start_byte = number(bytes.get("start"), "start byte offset")?;
    let mut normalized = Map::new();
    normalized.insert("path".to_owned(), Value::String(stable_path(file, workdir)));
    if let Some(language) = raw.get("language").and_then(Value::as_str) {
        normalized.insert(
            "language".to_owned(),
            Value::String(language.to_lowercase()),
        );
    }
    normalized.insert("text".to_owned(), Value::from(text));
    let start = point(range.get("start"), start_byte)?;
    let end = point(
        range.get("end"),
        number(bytes.get("end"), "end byte offset")?,
    )?;
    normalized.insert("range".to_owned(), json!({ "start": start, "end": end }));
    normalized.insert(
        "metavariables".to_owned(),
        normalize_metavariables(
            raw.get("metaVariables"),
            text,
            start_byte.as_f64().unwrap_or(0.0),
        )?,
    );
    for (key, value) in raw {
        if !CONSUMED.contains(&key.as_str()) {
            normalized.insert(key.clone(), value.clone());
        }
    }
    Ok(normalized)
}

fn start_offset(record: &Map<String, Value>) -> f64 {
    record
        .get("range")
        .and_then(|range| range.pointer("/start/byteOffset"))
        .and_then(Value::as_f64)
        .unwrap_or(0.0)
}

pub fn normalize_records(
    records: &[Map<String, Value>],
    workdir: &str,
) -> Result<Vec<Map<String, Value>>, NormalizeError> {
    let mut normalized = records
        .iter()
        .map(|record| normalize_match(record, workdir))
        .collect::<Result<Vec<_>, _>>()?;
    normalized.sort_by(|left, right| {
        let left_path = left.get("path").and_then(Value::as_str).unwrap_or_default();
        let right_path = right
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or_default();
        left_path.cmp(right_path).then_with(|| {
            start_offset(left)
                .partial_cmp(&start_offset(right))
                .unwrap_or(Ordering::Equal)
        })
    });
    Ok(normalized)
}
