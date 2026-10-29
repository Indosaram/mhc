//! Schema validation kernel.
//!
//! The TypeScript package validates every config surface with zod schema trees
//! and its tests assert on zod issues: `unrecognized_keys` carrying the
//! offending key list, `custom` refinements, and nested document paths such as
//! `dag.max_nodes_per_run`. A derive-only serde port cannot reproduce those
//! issues, so this module ports the schema tree itself: [`Node`] mirrors the zod
//! constructors, and [`parse`] walks it and returns the normalized document
//! (defaults materialized, legacy aliases folded).

use serde_json::{Map, Value};

use crate::issue::{self, Issues};

pub type PreprocessFn = fn(&Value) -> Value;
pub type RefineFn = fn(&Value, &[String], &mut Issues);
pub type DefaultFn = fn() -> Value;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Pattern {
    LowerSlug,
}

impl Pattern {
    fn matches(self, text: &str) -> bool {
        match self {
            Pattern::LowerSlug => {
                !text.is_empty()
                    && text
                        .chars()
                        .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
            }
        }
    }

    fn source(self) -> &'static str {
        match self {
            Pattern::LowerSlug => "^[a-z0-9-]+$",
        }
    }
}

#[derive(Clone, Copy, Default)]
pub struct NumberBounds {
    pub min: Option<f64>,
    pub max: Option<f64>,
}

#[derive(Clone, Copy, Default)]
pub struct IntBounds {
    pub min: Option<i64>,
    pub max: Option<i64>,
    pub exclusive_min: bool,
}

#[derive(Clone, Copy, Default)]
pub struct StringRules {
    pub min_len: Option<usize>,
    pub pattern: Option<Pattern>,
}

#[derive(Clone, Default)]
pub struct ArraySpec {
    pub item: Option<Box<Node>>,
    pub min_len: Option<usize>,
    pub max_len: Option<usize>,
}

#[derive(Clone)]
pub struct Field {
    pub key: &'static str,
    pub node: Node,
    pub default: Option<DefaultFn>,
    pub required: bool,
}

#[derive(Clone, Default)]
pub struct ObjectSpec {
    pub fields: Vec<Field>,
    pub strict: bool,
    pub preprocess: Option<PreprocessFn>,
    pub refine: Option<RefineFn>,
}

impl ObjectSpec {
    pub fn partial(&self) -> ObjectSpec {
        ObjectSpec {
            fields: self
                .fields
                .iter()
                .map(|field| Field {
                    key: field.key,
                    node: field.node.clone(),
                    default: None,
                    required: false,
                })
                .collect(),
            strict: self.strict,
            preprocess: self.preprocess,
            refine: None,
        }
    }
}

#[derive(Clone)]
pub enum Node {
    Any,
    Null,
    Boolean,
    Number(NumberBounds),
    Integer(IntBounds),
    String(StringRules),
    Literal(&'static str),
    LiteralNumber(i64),
    Enumeration(&'static [&'static str]),
    Array(ArraySpec),
    Record(Box<Node>),
    Object(Box<ObjectSpec>),
    Union(Vec<Node>),
}

impl Node {
    pub fn label(&self) -> &'static str {
        match self {
            Node::Any => "unknown",
            Node::Null => "null",
            Node::Boolean => "boolean",
            Node::Number(_) | Node::Integer(_) => "number",
            Node::String(_) => "string",
            Node::Literal(_) | Node::LiteralNumber(_) => "literal",
            Node::Enumeration(_) => "enum",
            Node::Array(_) => "array",
            Node::Record(_) => "record",
            Node::Object(_) => "object",
            Node::Union(_) => "union",
        }
    }

    pub fn with_min(self, min: f64) -> Node {
        match self {
            Node::Number(bounds) => Node::Number(NumberBounds {
                min: Some(min),
                ..bounds
            }),
            Node::Integer(bounds) => Node::Integer(IntBounds {
                min: Some(min as i64),
                ..bounds
            }),
            other => other,
        }
    }

    pub fn with_max(self, max: f64) -> Node {
        match self {
            Node::Number(bounds) => Node::Number(NumberBounds {
                max: Some(max),
                ..bounds
            }),
            Node::Integer(bounds) => Node::Integer(IntBounds {
                max: Some(max as i64),
                ..bounds
            }),
            other => other,
        }
    }
}

pub fn any() -> Node {
    Node::Any
}

pub fn boolean() -> Node {
    Node::Boolean
}

pub fn number() -> Node {
    Node::Number(NumberBounds::default())
}

pub fn number_between(min: f64, max: f64) -> Node {
    Node::Number(NumberBounds {
        min: Some(min),
        max: Some(max),
    })
}

pub fn integer() -> Node {
    Node::Integer(IntBounds::default())
}

pub fn positive_integer() -> Node {
    Node::Integer(IntBounds {
        min: Some(0),
        exclusive_min: true,
        max: None,
    })
}

pub fn nonnegative_integer() -> Node {
    Node::Integer(IntBounds {
        min: Some(0),
        exclusive_min: false,
        max: None,
    })
}

pub fn integer_between(min: i64, max: i64) -> Node {
    Node::Integer(IntBounds {
        min: Some(min),
        exclusive_min: false,
        max: Some(max),
    })
}

pub fn string() -> Node {
    Node::String(StringRules::default())
}

pub fn non_empty_string() -> Node {
    Node::String(StringRules {
        min_len: Some(1),
        pattern: None,
    })
}

pub fn lower_slug_string() -> Node {
    Node::String(StringRules {
        min_len: None,
        pattern: Some(Pattern::LowerSlug),
    })
}

pub fn literal(value: &'static str) -> Node {
    Node::Literal(value)
}

pub fn literal_number(value: i64) -> Node {
    Node::LiteralNumber(value)
}

pub fn enumeration(values: &'static [&'static str]) -> Node {
    Node::Enumeration(values)
}

pub fn array(item: Node) -> Node {
    Node::Array(ArraySpec {
        item: Some(Box::new(item)),
        min_len: None,
        max_len: None,
    })
}

pub fn array_bounded(item: Node, min_len: Option<usize>, max_len: Option<usize>) -> Node {
    Node::Array(ArraySpec {
        item: Some(Box::new(item)),
        min_len,
        max_len,
    })
}

pub fn record(value: Node) -> Node {
    Node::Record(Box::new(value))
}

pub fn strict_object(fields: Vec<Field>) -> Node {
    Node::Object(Box::new(ObjectSpec {
        fields,
        strict: true,
        ..ObjectSpec::default()
    }))
}

pub fn union(branches: Vec<Node>) -> Node {
    Node::Union(branches)
}

pub fn required(key: &'static str, node: Node) -> Field {
    Field {
        key,
        node,
        default: None,
        required: true,
    }
}

pub fn optional(key: &'static str, node: Node) -> Field {
    Field {
        key,
        node,
        default: None,
        required: false,
    }
}

pub fn defaulted(key: &'static str, node: Node, default: DefaultFn) -> Field {
    Field {
        key,
        node,
        default: Some(default),
        required: false,
    }
}

fn path_with(path: &[String], segment: &str) -> Vec<String> {
    let mut next = path.to_vec();
    next.push(segment.to_string());
    next
}

fn render_bound(value: f64) -> String {
    if value.fract() == 0.0 && value.abs() < 1e15 {
        format!("{}", value as i64)
    } else {
        format!("{value}")
    }
}

fn integral(value: &Value) -> Option<i64> {
    if let Some(number) = value.as_i64() {
        return Some(number);
    }
    let float = value.as_f64()?;
    if float.fract() == 0.0 && float.abs() < 9.007_199_254_740_992e15 {
        return Some(float as i64);
    }
    None
}

pub fn parse(node: &Node, value: &Value, path: &[String], issues: &mut Issues) -> Option<Value> {
    match node {
        Node::Any => Some(value.clone()),
        Node::Null => {
            if value.is_null() {
                Some(Value::Null)
            } else {
                issues.push(issue::invalid_type(path, "null", value));
                None
            }
        }
        Node::Boolean => match value {
            Value::Bool(flag) => Some(Value::Bool(*flag)),
            other => {
                issues.push(issue::invalid_type(path, "boolean", other));
                None
            }
        },
        Node::Number(bounds) => {
            let Some(number) = value.as_f64() else {
                issues.push(issue::invalid_type(path, "number", value));
                return None;
            };
            if let Some(min) = bounds.min
                && number < min
            {
                issues.push(issue::too_small(
                    path,
                    "number",
                    &format!(">={}", render_bound(min)),
                ));
                return None;
            }
            if let Some(max) = bounds.max
                && number > max
            {
                issues.push(issue::too_big(
                    path,
                    "number",
                    &format!("<={}", render_bound(max)),
                ));
                return None;
            }
            Some(value.clone())
        }
        Node::Integer(bounds) => {
            let Some(number) = integral(value) else {
                issues.push(issue::invalid_type(path, "int", value));
                return None;
            };
            if let Some(min) = bounds.min {
                let failed = if bounds.exclusive_min {
                    number <= min
                } else {
                    number < min
                };
                if failed {
                    let bound = if bounds.exclusive_min {
                        format!(">{}", render_bound(min as f64))
                    } else {
                        format!(">={}", render_bound(min as f64))
                    };
                    issues.push(issue::too_small(path, "number", &bound));
                    return None;
                }
            }
            if let Some(max) = bounds.max
                && number > max
            {
                issues.push(issue::too_big(
                    path,
                    "number",
                    &format!("<={}", render_bound(max as f64)),
                ));
                return None;
            }
            Some(Value::Number(number.into()))
        }
        Node::String(rules) => {
            let Value::String(text) = value else {
                issues.push(issue::invalid_type(path, "string", value));
                return None;
            };
            if let Some(min_len) = rules.min_len
                && text.chars().count() < min_len
            {
                issues.push(issue::too_small(
                    path,
                    "string",
                    &format!(">={min_len} characters"),
                ));
                return None;
            }
            if let Some(pattern) = rules.pattern
                && !pattern.matches(text)
            {
                issues.push(issue::invalid_format(path, pattern.source()));
                return None;
            }
            Some(value.clone())
        }
        Node::Literal(expected) => match value {
            Value::String(text) if text == expected => Some(value.clone()),
            other => {
                issues.push(issue::invalid_value(path, other, expected));
                None
            }
        },
        Node::LiteralNumber(expected) => match integral(value) {
            Some(number) if number == *expected => Some(value.clone()),
            _ => {
                issues.push(issue::invalid_value(path, value, &expected.to_string()));
                None
            }
        },
        Node::Enumeration(allowed) => match value {
            Value::String(text) if allowed.contains(&text.as_str()) => Some(value.clone()),
            other => {
                let options = allowed
                    .iter()
                    .map(|option| format!("\"{option}\""))
                    .collect::<Vec<_>>()
                    .join("|");
                issues.push(issue::invalid_value(path, other, &options));
                None
            }
        },
        Node::Array(spec) => {
            let Value::Array(items) = value else {
                issues.push(issue::invalid_type(path, "array", value));
                return None;
            };
            if let Some(min_len) = spec.min_len
                && items.len() < min_len
            {
                issues.push(issue::too_small(
                    path,
                    "array",
                    &format!(">={min_len} items"),
                ));
                return None;
            }
            if let Some(max_len) = spec.max_len
                && items.len() > max_len
            {
                issues.push(issue::too_big(path, "array", &format!("<={max_len} items")));
                return None;
            }
            let mut out: Vec<Value> = Vec::with_capacity(items.len());
            let mut failed = false;
            for (index, item) in items.iter().enumerate() {
                let item_path = path_with(path, &index.to_string());
                match &spec.item {
                    None => out.push(item.clone()),
                    Some(item_node) => match parse(item_node, item, &item_path, issues) {
                        Some(parsed) => out.push(parsed),
                        None => failed = true,
                    },
                }
            }
            if failed {
                return None;
            }
            Some(Value::Array(out))
        }
        Node::Record(inner) => {
            let Value::Object(map) = value else {
                issues.push(issue::invalid_type(path, "object", value));
                return None;
            };
            let mut out = Map::new();
            let mut failed = false;
            for (key, entry) in map {
                let entry_path = path_with(path, key);
                match parse(inner, entry, &entry_path, issues) {
                    Some(parsed) => {
                        out.insert(key.clone(), parsed);
                    }
                    None => failed = true,
                }
            }
            if failed {
                return None;
            }
            Some(Value::Object(out))
        }
        Node::Object(spec) => parse_object(spec, value, path, issues),
        Node::Union(branches) => {
            for branch in branches {
                let mut scratch: Issues = Vec::new();
                if let Some(parsed) = parse(branch, value, path, &mut scratch) {
                    return Some(parsed);
                }
            }
            issues.push(issue::invalid_union(path));
            None
        }
    }
}

fn parse_object(
    spec: &ObjectSpec,
    value: &Value,
    path: &[String],
    issues: &mut Issues,
) -> Option<Value> {
    let preprocessed = match spec.preprocess {
        Some(preprocess) => preprocess(value),
        None => value.clone(),
    };
    let Value::Object(map) = &preprocessed else {
        issues.push(issue::invalid_type(path, "object", &preprocessed));
        return None;
    };

    let mut out = Map::new();
    let mut failed = false;
    for field in &spec.fields {
        match map.get(field.key) {
            Some(entry) => {
                let entry_path = path_with(path, field.key);
                match parse(&field.node, entry, &entry_path, issues) {
                    Some(parsed) => {
                        out.insert(field.key.to_string(), parsed);
                    }
                    None => failed = true,
                }
            }
            None => {
                if let Some(default) = field.default {
                    out.insert(field.key.to_string(), default());
                } else if field.required {
                    issues.push(issue::invalid_type_missing(path, field.node.label()));
                    failed = true;
                }
            }
        }
    }

    if spec.strict {
        let unknown: Vec<String> = map
            .keys()
            .filter(|key| !spec.fields.iter().any(|field| field.key == key.as_str()))
            .cloned()
            .collect();
        if !unknown.is_empty() {
            issues.push(issue::unrecognized_keys(path, &unknown));
            failed = true;
        }
    }

    if failed {
        return None;
    }

    let parsed = Value::Object(out);
    if let Some(refine) = spec.refine {
        refine(&parsed, path, issues);
        if !issues.is_empty() {
            return None;
        }
    }
    Some(parsed)
}

pub fn safe_parse(node: &Node, value: &Value) -> Result<Value, Issues> {
    let mut issues: Issues = Vec::new();
    let parsed = parse(node, value, &[], &mut issues);
    match parsed {
        Some(parsed) if issues.is_empty() => Ok(parsed),
        _ => Err(issues),
    }
}
