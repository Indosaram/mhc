use serde_json::{Map, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IssueCode {
    Custom,
    InvalidFormat,
    InvalidType,
    InvalidUnion,
    InvalidValue,
    NotMultipleOf,
    TooBig,
    TooSmall,
    UnrecognizedKeys,
}

impl IssueCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            IssueCode::Custom => "custom",
            IssueCode::InvalidFormat => "invalid_format",
            IssueCode::InvalidType => "invalid_type",
            IssueCode::InvalidUnion => "invalid_union",
            IssueCode::InvalidValue => "invalid_value",
            IssueCode::NotMultipleOf => "not_multiple_of",
            IssueCode::TooBig => "too_big",
            IssueCode::TooSmall => "too_small",
            IssueCode::UnrecognizedKeys => "unrecognized_keys",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    pub code: IssueCode,
    pub keys: Vec<String>,
    pub message: String,
    pub path: Vec<String>,
}

impl Issue {
    pub fn path_string(&self) -> String {
        self.path.join(".")
    }
}

pub type Issues = Vec<Issue>;
pub type Validated<T> = Result<T, Issues>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathSegment {
    Key(String),
    Index(i64),
}

impl PathSegment {
    pub fn key(value: impl Into<String>) -> Self {
        PathSegment::Key(value.into())
    }

    pub fn as_path_string(&self) -> String {
        match self {
            PathSegment::Key(key) => key.clone(),
            PathSegment::Index(index) => index.to_string(),
        }
    }
}

pub fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

pub fn field(path: &[String], key: &str) -> Vec<String> {
    let mut next = path.to_vec();
    next.push(key.to_string());
    next
}

pub fn element(path: &[String], index: usize) -> Vec<String> {
    let mut next = path.to_vec();
    next.push(index.to_string());
    next
}

pub fn invalid_type(path: &[String], expected: &str, received: &Value) -> Issue {
    Issue {
        code: IssueCode::InvalidType,
        keys: Vec::new(),
        message: format!(
            "Invalid input: expected {expected}, received {}",
            type_name(received)
        ),
        path: path.to_vec(),
    }
}

pub fn invalid_type_missing(path: &[String], expected: &str) -> Issue {
    Issue {
        code: IssueCode::InvalidType,
        keys: Vec::new(),
        message: format!("Invalid input: expected {expected}, received undefined"),
        path: path.to_vec(),
    }
}

pub fn invalid_format(path: &[String], pattern: &str) -> Issue {
    Issue {
        code: IssueCode::InvalidFormat,
        keys: Vec::new(),
        message: format!("Invalid string: must match pattern /{pattern}/"),
        path: path.to_vec(),
    }
}

pub fn invalid_value(path: &[String], received: &Value, expected: &str) -> Issue {
    Issue {
        code: IssueCode::InvalidValue,
        keys: Vec::new(),
        message: format!("Invalid option: expected {expected}, received {received}"),
        path: path.to_vec(),
    }
}

pub fn invalid_union(path: &[String]) -> Issue {
    Issue {
        code: IssueCode::InvalidUnion,
        keys: Vec::new(),
        message: "Invalid input".to_string(),
        path: path.to_vec(),
    }
}

pub fn too_small(path: &[String], origin: &str, minimum: &str) -> Issue {
    Issue {
        code: IssueCode::TooSmall,
        keys: Vec::new(),
        message: format!("Too small: expected {origin} to be {minimum}"),
        path: path.to_vec(),
    }
}

pub fn too_big(path: &[String], origin: &str, maximum: &str) -> Issue {
    Issue {
        code: IssueCode::TooBig,
        keys: Vec::new(),
        message: format!("Too big: expected {origin} to be {maximum}"),
        path: path.to_vec(),
    }
}

pub fn unrecognized_keys(path: &[String], keys: &[String]) -> Issue {
    let rendered = keys
        .iter()
        .map(|key| format!("\"{key}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let label = if keys.len() == 1 {
        "Unrecognized key"
    } else {
        "Unrecognized keys"
    };
    Issue {
        code: IssueCode::UnrecognizedKeys,
        keys: keys.to_vec(),
        message: format!("{label}: {rendered}"),
        path: path.to_vec(),
    }
}

pub fn custom(path: &[String], message: impl Into<String>) -> Issue {
    Issue {
        code: IssueCode::Custom,
        keys: Vec::new(),
        message: message.into(),
        path: path.to_vec(),
    }
}

pub fn as_object<'a>(
    value: &'a Value,
    path: &[String],
    issues: &mut Issues,
) -> Option<&'a Map<String, Value>> {
    match value {
        Value::Object(map) => Some(map),
        other => {
            issues.push(invalid_type(path, "object", other));
            None
        }
    }
}

pub fn as_array<'a>(
    value: &'a Value,
    path: &[String],
    issues: &mut Issues,
) -> Option<&'a Vec<Value>> {
    match value {
        Value::Array(items) => Some(items),
        other => {
            issues.push(invalid_type(path, "array", other));
            None
        }
    }
}

pub fn as_string<'a>(value: &'a Value, path: &[String], issues: &mut Issues) -> Option<&'a str> {
    match value {
        Value::String(text) => Some(text.as_str()),
        other => {
            issues.push(invalid_type(path, "string", other));
            None
        }
    }
}

pub fn as_boolean(value: &Value, path: &[String], issues: &mut Issues) -> Option<bool> {
    match value {
        Value::Bool(flag) => Some(*flag),
        other => {
            issues.push(invalid_type(path, "boolean", other));
            None
        }
    }
}

pub fn as_number(value: &Value, path: &[String], issues: &mut Issues) -> Option<f64> {
    match value {
        Value::Number(number) => number.as_f64(),
        other => {
            issues.push(invalid_type(path, "number", other));
            None
        }
    }
}

pub fn as_integer(value: &Value, path: &[String], issues: &mut Issues) -> Option<i64> {
    match value {
        Value::Number(number) => number.as_i64(),
        other => {
            issues.push(invalid_type(path, "int", other));
            None
        }
    }
}

pub fn as_string_array(value: &Value, path: &[String], issues: &mut Issues) -> Option<Vec<String>> {
    let items = as_array(value, path, issues)?;
    let mut out = Vec::with_capacity(items.len());
    for (index, item) in items.iter().enumerate() {
        let item_path = element(path, index);
        let text = as_string(item, &item_path, issues)?;
        out.push(text.to_string());
    }
    Some(out)
}

pub fn as_boolean_record(
    value: &Value,
    path: &[String],
    issues: &mut Issues,
) -> Option<Map<String, Value>> {
    let map = as_object(value, path, issues)?;
    let mut out = Map::new();
    for (key, entry) in map {
        let entry_path = field(path, key);
        let flag = as_boolean(entry, &entry_path, issues)?;
        out.insert(key.clone(), Value::Bool(flag));
    }
    Some(out)
}

#[macro_export]
macro_rules! collect_child {
    ($issues:ident, $expr:expr, $fallback:expr) => {
        match $expr {
            Ok(value) => value,
            Err(mut child_issues) => {
                $issues.append(&mut child_issues);
                $fallback
            }
        }
    };
}

pub fn reject_unknown_keys(
    map: &Map<String, Value>,
    allowed: &[&str],
    path: &[String],
    issues: &mut Issues,
) {
    let unknown: Vec<String> = map
        .keys()
        .filter(|key| !allowed.contains(&key.as_str()))
        .cloned()
        .collect();
    if !unknown.is_empty() {
        issues.push(unrecognized_keys(path, &unknown));
    }
}
