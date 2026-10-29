//! Argument parsing and validation for the three tools (spec §3).

use crate::error::{ToolError, UNSUPPORTED_LANGUAGE};
use crate::error::Phase;
use serde_json::{Map, Value};

pub const LANGUAGES: [&str; 25] = [
    "bash",
    "c",
    "cpp",
    "csharp",
    "css",
    "elixir",
    "go",
    "haskell",
    "html",
    "java",
    "javascript",
    "json",
    "kotlin",
    "lua",
    "nix",
    "php",
    "python",
    "ruby",
    "rust",
    "scala",
    "solidity",
    "swift",
    "typescript",
    "tsx",
    "yaml",
];
pub const STRICTNESS: [&str; 5] = ["cst", "smart", "ast", "relaxed", "signature"];

pub const PATTERN_MAX_BYTES: usize = 16 * 1024;
pub const REWRITE_MAX_BYTES: usize = 64 * 1024;
pub const RULES_MAX_BYTES: usize = 64 * 1024;
pub const DEFAULT_MAX_MATCHES: u64 = 50;
pub const MAX_MATCHES: u64 = 500;
pub const MIN_TIMEOUT_MS: u64 = 1000;
pub const MAX_TIMEOUT_MS: u64 = 300_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Search,
    Rewrite,
    Scan,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Search => "search",
            Self::Rewrite => "rewrite",
            Self::Scan => "scan",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "search" => Some(Self::Search),
            "rewrite" => Some(Self::Rewrite),
            "scan" => Some(Self::Scan),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RuleSource {
    File(String),
    Inline(String),
}

/// A validated tool call.
#[derive(Clone, Debug)]
pub struct Request {
    pub kind: Kind,
    pub paths: Vec<String>,
    pub workdir: Option<String>,
    pub globs: Vec<String>,
    pub max_matches: usize,
    pub timeout_ms: u64,
    pub include_hidden: bool,
    pub follow_symlinks: bool,
    pub pattern: String,
    pub language: String,
    pub selector: Option<String>,
    pub strictness: String,
    pub force: bool,
    pub rewrite: Option<String>,
    pub apply: bool,
    pub rule: Option<RuleSource>,
    pub include_metadata: bool,
}

fn allowed_keys(kind: Kind) -> &'static [&'static str] {
    match kind {
        Kind::Search => &[
            "paths", "workdir", "globs", "maxMatches", "timeoutMs", "includeHidden",
            "followSymlinks", "pattern", "language", "selector", "strictness", "force",
        ],
        Kind::Rewrite => &[
            "paths", "workdir", "globs", "maxMatches", "timeoutMs", "includeHidden",
            "followSymlinks", "pattern", "language", "selector", "strictness", "force", "rewrite",
            "apply",
        ],
        Kind::Scan => &[
            "paths", "workdir", "globs", "maxMatches", "timeoutMs", "includeHidden",
            "followSymlinks", "ruleFile", "inlineRules", "includeMetadata", "apply",
        ],
    }
}

fn str_arg<'a>(
    map: &'a Map<String, Value>,
    key: &str,
    min: usize,
    max_chars: usize,
) -> Result<Option<&'a str>, ToolError> {
    match map.get(key) {
        None => Ok(None),
        Some(Value::String(text)) => {
            let chars = text.chars().count();
            if chars < min || chars > max_chars {
                return Err(ToolError::invalid(format!(
                    "`{key}` must be {min}..{max_chars} characters long."
                )));
            }
            Ok(Some(text))
        }
        Some(_) => Err(ToolError::invalid(format!("`{key}` must be a string."))),
    }
}

fn bool_arg(map: &Map<String, Value>, key: &str) -> Result<bool, ToolError> {
    match map.get(key) {
        None => Ok(false),
        Some(Value::Bool(value)) => Ok(*value),
        Some(_) => Err(ToolError::invalid(format!("`{key}` must be a boolean."))),
    }
}

fn int_arg(
    map: &Map<String, Value>,
    key: &str,
    min: u64,
    max: u64,
    default: u64,
) -> Result<u64, ToolError> {
    let Some(value) = map.get(key) else {
        return Ok(default);
    };
    let integer = value.as_u64().or_else(|| {
        value
            .as_f64()
            .filter(|number| number.fract() == 0.0 && *number >= 0.0 && *number < 1e15)
            .map(|number| number as u64)
    });
    match integer {
        Some(number) if (min..=max).contains(&number) => Ok(number),
        _ => Err(ToolError::invalid(format!(
            "`{key}` must be an integer between {min} and {max}."
        ))),
    }
}

fn string_list(
    map: &Map<String, Value>,
    key: &str,
    min_items: usize,
    max_items: usize,
    max_chars: usize,
) -> Result<Vec<String>, ToolError> {
    let Some(value) = map.get(key) else {
        if min_items > 0 {
            return Err(ToolError::invalid(format!("`{key}` is required.")));
        }
        return Ok(Vec::new());
    };
    let Some(items) = value.as_array() else {
        return Err(ToolError::invalid(format!(
            "`{key}` must be an array of strings."
        )));
    };
    if items.len() < min_items || items.len() > max_items {
        return Err(ToolError::invalid(format!(
            "`{key}` must have {min_items}..{max_items} items."
        )));
    }
    let mut out = Vec::new();
    for item in items {
        match item.as_str() {
            Some(text) if (1..=max_chars).contains(&text.chars().count()) => {
                out.push(text.to_owned());
            }
            _ => {
                return Err(ToolError::invalid(format!(
                    "each `{key}` item must be a string of 1..{max_chars} characters."
                )));
            }
        }
    }
    Ok(out)
}

/// Validate `arguments` for `kind`. Everything here is `preflight`.
pub fn parse(kind: Kind, arguments: Option<&Value>) -> Result<Request, ToolError> {
    let map = match arguments {
        Some(Value::Object(map)) => map,
        _ => return Err(ToolError::invalid("The arguments must be a JSON object.")),
    };
    let allowed = allowed_keys(kind);
    let unknown: Vec<&String> = map.keys().filter(|key| !allowed.contains(&key.as_str())).collect();
    if let Some(first) = unknown.first() {
        return Err(ToolError::invalid(format!("Unknown property `{first}`."))
            .with_details(serde_json::json!({ "unknown": unknown })));
    }

    let paths = string_list(map, "paths", 1, 64, 4096)?;
    let workdir = str_arg(map, "workdir", 1, 4096)?.map(str::to_owned);
    let globs = string_list(map, "globs", 0, 32, 1024)?;
    let max_matches = int_arg(map, "maxMatches", 1, MAX_MATCHES, DEFAULT_MAX_MATCHES)? as usize;
    let timeout_ms = int_arg(map, "timeoutMs", MIN_TIMEOUT_MS, MAX_TIMEOUT_MS, MAX_TIMEOUT_MS)?;
    let include_hidden = bool_arg(map, "includeHidden")?;
    let follow_symlinks = bool_arg(map, "followSymlinks")?;
    let apply = bool_arg(map, "apply")?;

    let mut request = Request {
        kind,
        paths,
        workdir,
        globs,
        max_matches,
        timeout_ms,
        include_hidden,
        follow_symlinks,
        pattern: String::new(),
        language: String::new(),
        selector: None,
        strictness: "smart".to_owned(),
        force: false,
        rewrite: None,
        apply,
        rule: None,
        include_metadata: false,
    };

    if kind == Kind::Scan {
        let file = str_arg(map, "ruleFile", 1, 4096)?;
        let inline = str_arg(map, "inlineRules", 1, usize::MAX)?;
        request.rule = match (file, inline) {
            (Some(file), None) => Some(RuleSource::File(file.to_owned())),
            (None, Some(text)) => {
                if text.len() > RULES_MAX_BYTES {
                    return Err(ToolError::invalid(
                        "`inlineRules` must be at most 65536 bytes of UTF-8.",
                    ));
                }
                Some(RuleSource::Inline(text.to_owned()))
            }
            _ => {
                return Err(ToolError::invalid(
                    "Give exactly one of `ruleFile` and `inlineRules`.",
                ));
            }
        };
        request.include_metadata = bool_arg(map, "includeMetadata")?;
        return Ok(request);
    }

    let pattern = match map.get("pattern") {
        Some(Value::String(text)) => text,
        Some(_) => return Err(ToolError::invalid("`pattern` must be a string.")),
        None => return Err(ToolError::invalid("`pattern` is required.")),
    };
    if pattern.trim().is_empty() {
        return Err(ToolError::invalid("`pattern` must not be blank."));
    }
    if pattern.len() > PATTERN_MAX_BYTES {
        return Err(ToolError::invalid(
            "`pattern` must be at most 16384 bytes of UTF-8.",
        ));
    }
    let language = match map.get("language") {
        Some(Value::String(text)) => text,
        Some(_) => return Err(ToolError::invalid("`language` must be a string.")),
        None => return Err(ToolError::invalid("`language` is required.")),
    };
    if !LANGUAGES.contains(&language.as_str()) {
        return Err(ToolError::new(
            UNSUPPORTED_LANGUAGE,
            Phase::Preflight,
            format!("Unsupported language `{language}`."),
        )
        .with_details(serde_json::json!({ "supported": LANGUAGES })));
    }
    request.pattern = pattern.clone();
    request.language = language.clone();
    request.selector = str_arg(map, "selector", 1, 128)?.map(str::to_owned);
    if let Some(value) = map.get("strictness") {
        match value.as_str() {
            Some(text) if STRICTNESS.contains(&text) => request.strictness = text.to_owned(),
            _ => {
                return Err(ToolError::invalid(
                    "`strictness` must be one of cst, smart, ast, relaxed, signature.",
                ));
            }
        }
    }
    request.force = bool_arg(map, "force")?;
    if kind == Kind::Rewrite {
        match map.get("rewrite") {
            Some(Value::String(text)) => {
                if text.len() > REWRITE_MAX_BYTES {
                    return Err(ToolError::invalid(
                        "`rewrite` must be at most 65536 bytes of UTF-8.",
                    ));
                }
                request.rewrite = Some(text.clone());
            }
            Some(_) => return Err(ToolError::invalid("`rewrite` must be a string.")),
            None => return Err(ToolError::invalid("`rewrite` is required.")),
        }
    }
    Ok(request)
}
