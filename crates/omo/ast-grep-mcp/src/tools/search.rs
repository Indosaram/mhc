use serde_json::Map;
use serde_json::Value;
use serde_json::json;

use super::LANGUAGES;
use super::MAX_GLOBS;
use super::MAX_PATHS;
use super::STRICTNESS;
use super::as_object;
use super::code_points;
use super::distinct_paths;
use super::optional_bool;
use super::scope_args;
use super::string_list;
use crate::abort::AbortSignal;
use crate::normalize::normalize_records;
use crate::pattern_hints::HintSeverity;
use crate::pattern_hints::ValidationOpts;
use crate::pattern_hints::validate_pattern_hints;
use crate::sg_runner::SgRunnerInput;
use crate::sg_runner::reason_value;
use crate::sg_runner::spawn_sg_runner;

pub const MAX_PATTERN_BYTES: usize = 16 * 1024;
const MAX_PATH_LEN: usize = 4096;
const MAX_GLOB_LEN: usize = 1024;
const MAX_SELECTOR_LEN: usize = 128;
const MAX_WORKDIR_LEN: usize = 4096;

pub const SEARCH_TOOL_NAME: &str = "search";
pub const SEARCH_TOOL_DESCRIPTION: &str = "Search code structurally with ast-grep. The pattern is code, not regex, and must parse as one AST node in the required language; use narrow paths. `$NAME` and `$_` match one whole node, while `$$$NAME` and `$$$` match zero-or-more nodes. Names are uppercase, `$$NAME` is invalid, partial-token captures do not work, and a repeated metavariable must match identical code. Wrap non-standalone syntax and use `selector` when needed. Parse warnings mean the query failed, not that the code is absent.";

const LIMIT_WARNING: &str = "Result limit reached; narrow paths or globs.";
const RETRYABLE_CODES: [&str; 4] = [
    "TIMEOUT",
    "ABORTED",
    "OUTPUT_PARSE_FAILED",
    "REWRITE_STALE_PREVIEW",
];
const KNOWN: [&str; 12] = [
    "pattern",
    "language",
    "paths",
    "workdir",
    "globs",
    "selector",
    "strictness",
    "maxMatches",
    "timeoutMs",
    "includeHidden",
    "followSymlinks",
    "force",
];

#[derive(Debug, Clone, PartialEq)]
pub struct SearchInput {
    pub pattern: String,
    pub language: String,
    pub paths: Vec<String>,
    pub workdir: Option<String>,
    pub globs: Option<Vec<String>>,
    pub selector: Option<String>,
    pub strictness: String,
    pub max_matches: u64,
    pub timeout_ms: u64,
    pub include_hidden: Option<bool>,
    pub follow_symlinks: Option<bool>,
    pub force: Option<bool>,
}

impl SearchInput {
    pub fn to_value(&self) -> Value {
        let mut out = Map::new();
        out.insert("pattern".into(), json!(self.pattern));
        out.insert("language".into(), json!(self.language));
        out.insert("paths".into(), json!(self.paths));
        let optional = [
            ("workdir", self.workdir.as_ref().map(|v| json!(v))),
            ("globs", self.globs.as_ref().map(|v| json!(v))),
            ("selector", self.selector.as_ref().map(|v| json!(v))),
        ];
        for (key, value) in optional.into_iter() {
            if let Some(value) = value {
                out.insert(key.into(), value);
            }
        }
        out.insert("strictness".into(), json!(self.strictness));
        out.insert("maxMatches".into(), json!(self.max_matches));
        out.insert("timeoutMs".into(), json!(self.timeout_ms));
        for (key, value) in [
            ("includeHidden", self.include_hidden),
            ("followSymlinks", self.follow_symlinks),
            ("force", self.force),
        ] {
            if let Some(value) = value {
                out.insert(key.into(), json!(value));
            }
        }
        Value::Object(out)
    }
}

fn optional_text(
    obj: &Map<String, Value>,
    key: &str,
    max: usize,
) -> Result<Option<String>, String> {
    let Some(value) = obj.get(key) else {
        return Ok(None);
    };
    let text = value
        .as_str()
        .ok_or_else(|| format!("{key} must be a string"))?;
    if text.is_empty() {
        return Err(format!("{key} must be at least 1 character"));
    }
    if code_points(text) > max {
        return Err(format!("{key} must be at most {max} characters"));
    }
    Ok(Some(text.to_owned()))
}

pub fn parse_search_input(input: &Value) -> Result<SearchInput, String> {
    let obj = as_object(input)?;
    let pattern = obj
        .get("pattern")
        .and_then(Value::as_str)
        .ok_or("pattern must be a string")?;
    if pattern.is_empty() {
        return Err("pattern must be at least 1 character".into());
    }
    if pattern.len() > MAX_PATTERN_BYTES {
        return Err(format!("pattern must be at most {MAX_PATTERN_BYTES} bytes"));
    }
    let language = obj
        .get("language")
        .and_then(Value::as_str)
        .filter(|language| LANGUAGES.contains(language))
        .ok_or_else(|| format!("language must be one of: {}", LANGUAGES.join(", ")))?;
    if !obj.contains_key("paths") {
        return Err("paths must be an array".into());
    }
    let paths = string_list(obj, "paths", "path", 1, MAX_PATHS, MAX_PATH_LEN)?.unwrap_or_default();
    let workdir = optional_text(obj, "workdir", MAX_WORKDIR_LEN)?;
    let globs = string_list(obj, "globs", "glob", 0, MAX_GLOBS, MAX_GLOB_LEN)?;
    let selector = optional_text(obj, "selector", MAX_SELECTOR_LEN)?;
    let strictness = match obj.get("strictness") {
        None => "smart",
        Some(value) => value
            .as_str()
            .filter(|value| STRICTNESS.contains(value))
            .ok_or_else(|| format!("strictness must be one of: {}", STRICTNESS.join(", ")))?,
    };
    let max_matches = super::max_matches(obj)?;
    let timeout_ms = super::timeout_ms(obj)?;
    let include_hidden = optional_bool(obj, "includeHidden")?;
    let follow_symlinks = optional_bool(obj, "followSymlinks")?;
    let force = optional_bool(obj, "force")?;
    super::reject_unknown(obj, &KNOWN)?;
    Ok(SearchInput {
        pattern: pattern.to_owned(),
        language: language.to_owned(),
        paths,
        workdir,
        globs,
        selector,
        strictness: strictness.to_owned(),
        max_matches,
        timeout_ms,
        include_hidden,
        follow_symlinks,
        force,
    })
}

pub fn build_search_args(input: &SearchInput) -> Vec<String> {
    let mut args: Vec<String> = [
        "run",
        "-p",
        &input.pattern,
        "--lang",
        &input.language,
        "--json=stream",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    args.push("--strictness".into());
    args.push(input.strictness.clone());
    if let Some(selector) = &input.selector {
        args.push("--selector".into());
        args.push(selector.clone());
    }
    args.extend(scope_args(
        input.globs.as_deref(),
        input.include_hidden.unwrap_or(false),
        input.follow_symlinks.unwrap_or(false),
    ));
    args.extend(input.paths.iter().cloned());
    args
}

fn make_error(
    code: &str,
    message: &str,
    language: &str,
    phase: &str,
    stderr: &str,
    hint: &str,
) -> Value {
    json!({
        "schemaVersion": 1,
        "ok": false,
        "error": {
            "code": code,
            "message": message,
            "retryable": RETRYABLE_CODES.contains(&code),
            "phase": phase,
            "language": language,
            "details": { "stderr": stderr, "hint": hint },
        },
    })
}

pub fn execute_search(input: &SearchInput, sg_path: &str, signal: Option<&AbortSignal>) -> Value {
    let workdir = input.workdir.clone().unwrap_or_else(|| {
        std::env::current_dir()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_else(|_| ".".to_owned())
    });
    let paths = json!(input.paths);
    let validation = validate_pattern_hints(
        &input.pattern,
        &input.language,
        &ValidationOpts {
            force: input.force.unwrap_or(false),
            paths: Some(&paths),
            limit: Some(input.max_matches as f64),
        },
    );
    if validation.rejected {
        let message = validation
            .hints
            .iter()
            .find(|hint| {
                matches!(
                    hint.severity,
                    HintSeverity::AlwaysReject | HintSeverity::Reject
                )
            })
            .map_or("Pattern validation failed", |hint| hint.message.as_str());
        let hint = validation
            .hints
            .iter()
            .map(|hint| hint.message.as_str())
            .collect::<Vec<_>>()
            .join("; ");
        return make_error(
            validation.code.unwrap_or("INVALID_ARGUMENT"),
            message,
            &input.language,
            "preflight",
            "",
            &hint,
        );
    }

    let result = spawn_sg_runner(SgRunnerInput {
        sg_path,
        args: build_search_args(input),
        workdir: &workdir,
        env: None,
        max_matches: Some(input.max_matches),
        timeout_ms: Some(input.timeout_ms),
        signal,
    });
    let result = match result {
        Ok(result) => result,
        Err(error) => {
            return make_error(
                error.code.as_str(),
                &error.message,
                &input.language,
                "search",
                &error.stderr,
                "",
            );
        }
    };
    if result
        .stderr
        .to_lowercase()
        .contains("pattern contains an error node")
    {
        return make_error(
            "PATTERN_PARSE_FAILED",
            &format!("Pattern did not parse as one {} AST node.", input.language),
            &input.language,
            "search",
            &result.stderr,
            "Use a complete function, call, declaration, or wrapped context.",
        );
    }
    let mut warnings: Vec<String> = validation
        .hints
        .iter()
        .map(|hint| hint.message.clone())
        .collect();
    let matches = match normalize_records(&result.records, &workdir) {
        Ok(matches) => matches,
        Err(error) => {
            return make_error("SG_FAILED", &error.0, &input.language, "search", "", "");
        }
    };
    let returned_files = distinct_paths(&matches);
    let truncated = result.truncated;
    if truncated {
        warnings.push(LIMIT_WARNING.to_owned());
    }
    json!({
        "schemaVersion": 1,
        "ok": true,
        "kind": "search",
        "workdir": workdir,
        "counts": {
            "returnedMatches": matches.len(),
            "returnedFiles": returned_files,
            "totalMatches": if truncated { Value::Null } else { json!(matches.len()) },
            "totalFiles": if truncated { Value::Null } else { json!(returned_files) },
            "atLeastMatches": result.at_least_matches,
        },
        "matches": matches,
        "truncation": {
            "truncated": truncated,
            "reason": reason_value(result.reason),
            "maxMatches": input.max_matches,
            "maxPayloadBytes": result.max_payload_bytes,
            "salvagedRecords": result.salvaged_records,
        },
        "warnings": warnings,
        "durationMs": result.duration_ms,
    })
}
