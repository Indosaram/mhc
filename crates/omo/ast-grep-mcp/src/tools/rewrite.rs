use std::time::Instant;

use serde_json::Map;
use serde_json::Value;
use serde_json::json;

use super::LANGUAGES;
use super::MAX_GLOBS;
use super::MAX_PATHS;
use super::STRICTNESS;
use super::as_object;
use super::code_points;
use super::default_workdir;
use super::distinct_paths;
use super::elapsed_ms;
use super::optional_bool;
use super::scope_args;
use super::string_list;
use crate::abort::AbortSignal;
use crate::normalize::normalize_records;
use crate::pattern_hints::HintSeverity;
use crate::pattern_hints::ValidationOpts;
use crate::pattern_hints::validate_rewrite_hints;
use crate::sg_runner::SgRunnerInput;
use crate::sg_runner::reason_value;
use crate::sg_runner::spawn_sg_runner;

pub const MAX_PATTERN_BYTES: usize = 16 * 1024;
pub const MAX_REWRITE_BYTES: usize = 64 * 1024;
pub const MAX_PATH_CHARS: usize = 4096;
pub const MAX_GLOB_CHARS: usize = 1024;
pub const MAX_SELECTOR_CHARS: usize = 128;
pub const MAX_WORKDIR_CHARS: usize = 4096;

pub const REWRITE_TOOL_NAME: &str = "rewrite";
pub const REWRITE_TOOL_DESCRIPTION: &str = "Preview or apply an AST-aware rewrite. The pattern follows the same metavariable rules as `search`; the replacement may only reference metavariables captured by the pattern, and an empty replacement deletes the match. Dry-run is the default. Apply uses a JSON preview followed by a separate `--update-all` process because `sg` cannot safely combine JSON output and mutation. Truncated previews are never applied, and rewrite idempotency is not guaranteed.";
pub const APPLY_PREVIEW_WARNING: &str =
    "Mutation counts are based on the preview pass; sg update-all does not return equivalent JSON.";
pub const NOTHING_TO_APPLY_WARNING: &str = "Nothing to apply: the preview found no matches.";

const RETRYABLE: [&str; 4] = [
    "ABORTED",
    "OUTPUT_PARSE_FAILED",
    "REWRITE_STALE_PREVIEW",
    "TIMEOUT",
];
const KNOWN_KEYS: [&str; 14] = [
    "pattern",
    "rewrite",
    "language",
    "paths",
    "workdir",
    "globs",
    "selector",
    "strictness",
    "apply",
    "maxMatches",
    "timeoutMs",
    "includeHidden",
    "followSymlinks",
    "force",
];

#[derive(Debug, Clone, PartialEq)]
pub struct RewriteInput {
    pub pattern: String,
    pub rewrite: String,
    pub language: String,
    pub paths: Vec<String>,
    pub workdir: Option<String>,
    pub globs: Option<Vec<String>>,
    pub selector: Option<String>,
    pub strictness: String,
    pub apply: bool,
    pub max_matches: u64,
    pub timeout_ms: u64,
    pub include_hidden: Option<bool>,
    pub follow_symlinks: Option<bool>,
    pub force: Option<bool>,
}

#[derive(Default)]
pub struct RewriteHooks<'a> {
    pub on_preview_complete: Option<&'a dyn Fn()>,
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
        .filter(|text| !text.is_empty())
        .ok_or_else(|| format!("{key} must be a non-empty string"))?;
    if code_points(text) > max {
        return Err(format!("{key} must be at most {max} characters"));
    }
    Ok(Some(text.to_owned()))
}

pub fn parse_rewrite_input(input: &Value) -> Result<RewriteInput, String> {
    let obj = as_object(input)?;
    super::reject_unknown(obj, &KNOWN_KEYS)?;
    let pattern = obj
        .get("pattern")
        .and_then(Value::as_str)
        .filter(|pattern| !pattern.is_empty())
        .ok_or("pattern must be a non-empty string")?;
    if pattern.len() > MAX_PATTERN_BYTES {
        return Err("pattern must be at most 16KiB".into());
    }
    let rewrite = obj
        .get("rewrite")
        .and_then(Value::as_str)
        .ok_or("rewrite must be a string")?;
    if rewrite.len() > MAX_REWRITE_BYTES {
        return Err("rewrite must be at most 64KiB".into());
    }
    let language = obj
        .get("language")
        .and_then(Value::as_str)
        .filter(|language| LANGUAGES.contains(language))
        .ok_or_else(|| format!("language must be one of: {}", LANGUAGES.join(", ")))?;
    if !obj.contains_key("paths") {
        return Err("paths must be an array".into());
    }
    let paths =
        string_list(obj, "paths", "path", 1, MAX_PATHS, MAX_PATH_CHARS)?.unwrap_or_default();
    let workdir = optional_text(obj, "workdir", MAX_WORKDIR_CHARS)?;
    let globs = string_list(obj, "globs", "glob", 0, MAX_GLOBS, MAX_GLOB_CHARS)?;
    let selector = optional_text(obj, "selector", MAX_SELECTOR_CHARS)?;
    let strictness = match obj.get("strictness") {
        None => "smart",
        Some(value) => value
            .as_str()
            .filter(|value| STRICTNESS.contains(value))
            .ok_or_else(|| format!("strictness must be one of: {}", STRICTNESS.join(", ")))?,
    };
    let apply = optional_bool(obj, "apply")?;
    let max_matches = super::max_matches(obj)?;
    let timeout_ms = super::timeout_ms(obj)?;
    let include_hidden = optional_bool(obj, "includeHidden")?;
    let follow_symlinks = optional_bool(obj, "followSymlinks")?;
    let force = optional_bool(obj, "force")?;
    Ok(RewriteInput {
        pattern: pattern.to_owned(),
        rewrite: rewrite.to_owned(),
        language: language.to_owned(),
        paths,
        workdir,
        globs,
        selector,
        strictness: strictness.to_owned(),
        apply: apply.unwrap_or(false),
        max_matches,
        timeout_ms,
        include_hidden,
        follow_symlinks,
        force,
    })
}

fn rewrite_scope_args(input: &RewriteInput) -> Vec<String> {
    let mut args = vec!["--strictness".to_owned(), input.strictness.clone()];
    if let Some(selector) = &input.selector {
        args.push("--selector".into());
        args.push(selector.clone());
    }
    args.extend(scope_args(
        input.globs.as_deref(),
        input.include_hidden.unwrap_or(false),
        input.follow_symlinks.unwrap_or(false),
    ));
    args
}

fn base_args(input: &RewriteInput) -> Vec<String> {
    [
        "run",
        "-p",
        &input.pattern,
        "-r",
        &input.rewrite,
        "--lang",
        &input.language,
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

pub fn build_rewrite_args(input: &RewriteInput) -> Vec<String> {
    let mut args = base_args(input);
    args.push("--json=stream".into());
    args.extend(rewrite_scope_args(input));
    args.extend(input.paths.iter().cloned());
    args
}

pub fn build_rewrite_apply_args(input: &RewriteInput) -> Vec<String> {
    let mut args = base_args(input);
    args.push("--update-all".into());
    args.extend(rewrite_scope_args(input));
    args.extend(input.paths.iter().cloned());
    args
}

#[derive(Default)]
struct Details {
    stderr: Option<String>,
    hints: Option<Vec<String>>,
}

fn failure(
    code: &str,
    message: &str,
    phase: &str,
    language: &str,
    duration_ms: u64,
    details: Details,
) -> Value {
    let mut detail_map = Map::new();
    if let Some(stderr) = details.stderr {
        detail_map.insert("stderr".into(), json!(stderr));
    }
    if let Some(hints) = details.hints {
        detail_map.insert("hints".into(), json!(hints));
    }
    json!({
        "schemaVersion": 1,
        "ok": false,
        "kind": "rewrite",
        "error": {
            "code": code,
            "message": message,
            "retryable": RETRYABLE.contains(&code),
            "phase": phase,
            "language": language,
            "details": detail_map,
        },
        "durationMs": duration_ms,
    })
}

fn stderr_details(stderr: &str) -> Details {
    Details {
        stderr: Some(stderr.to_owned()),
        hints: None,
    }
}

fn preflight_code(code: Option<&str>) -> &'static str {
    match code {
        Some("REWRITE_UNBOUND_METAVARIABLE") => "REWRITE_UNBOUND_METAVARIABLE",
        Some("REWRITE_CARDINALITY_MISMATCH") => "REWRITE_METAVARIABLE_KIND_MISMATCH",
        Some("LANGUAGE_UNSUPPORTED") => "UNSUPPORTED_LANGUAGE",
        Some("PATTERN_HINT_REJECTED") => "PATTERN_HINT_REJECTED",
        _ => "INVALID_ARGUMENT",
    }
}

fn to_rewrite_matches(
    records: &[Map<String, Value>],
    workdir: &str,
) -> Result<Vec<Map<String, Value>>, String> {
    let mut matches = normalize_records(records, workdir).map_err(|error| error.0)?;
    for m in &mut matches {
        let replacement = m
            .get("replacement")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        m.insert("replacement".into(), json!(replacement));
    }
    Ok(matches)
}

pub fn execute_rewrite(
    raw_input: &Value,
    sg_path: &str,
    signal: Option<&AbortSignal>,
    hooks: &RewriteHooks<'_>,
) -> Value {
    let started_at = Instant::now();
    let elapsed = || elapsed_ms(started_at);

    let input = match parse_rewrite_input(raw_input) {
        Ok(input) => input,
        Err(message) => {
            let language = raw_input
                .get("language")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            return failure(
                "INVALID_ARGUMENT",
                &message,
                "preflight",
                language,
                elapsed(),
                Details::default(),
            );
        }
    };
    let language = input.language.as_str();
    let workdir = input.workdir.clone().unwrap_or_else(default_workdir);

    let paths = json!(input.paths);
    let validation = validate_rewrite_hints(
        &input.pattern,
        &input.rewrite,
        language,
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
            .map_or("Rewrite preflight rejected the request.", |hint| {
                hint.message.as_str()
            });
        return failure(
            preflight_code(validation.code),
            message,
            "preflight",
            language,
            elapsed(),
            Details {
                stderr: None,
                hints: Some(
                    validation
                        .hints
                        .iter()
                        .map(|hint| hint.message.clone())
                        .collect(),
                ),
            },
        );
    }
    let warnings: Vec<String> = validation
        .hints
        .iter()
        .map(|hint| hint.message.clone())
        .collect();

    let preview = match spawn_sg_runner(SgRunnerInput {
        sg_path,
        args: build_rewrite_args(&input),
        workdir: &workdir,
        env: None,
        max_matches: Some(input.max_matches),
        timeout_ms: Some(input.timeout_ms),
        signal,
    }) {
        Ok(preview) => preview,
        Err(error) => {
            return failure(
                error.code.as_str(),
                &error.message,
                "preview",
                language,
                error.duration_ms,
                stderr_details(&error.stderr),
            );
        }
    };

    if preview
        .stderr
        .to_lowercase()
        .contains("pattern contains an error node")
    {
        return failure(
            "PATTERN_PARSE_FAILED",
            "ast-grep reported an ERROR node while parsing the pattern; the query failed rather than finding nothing.",
            "preview",
            language,
            elapsed(),
            stderr_details(&preview.stderr),
        );
    }

    let matches = match to_rewrite_matches(&preview.records, &workdir) {
        Ok(matches) => matches,
        Err(message) => {
            return failure(
                "SG_FAILED",
                &message,
                "preview",
                language,
                elapsed(),
                Details::default(),
            );
        }
    };
    let truncation = json!({
        "truncated": preview.truncated,
        "reason": reason_value(preview.reason),
        "maxMatches": input.max_matches,
        "maxPayloadBytes": preview.max_payload_bytes,
        "salvagedRecords": preview.salvaged_records,
    });
    let counts = json!({
        "plannedMatches": matches.len(),
        "plannedFiles": distinct_paths(&matches),
    });

    let success = |applied: bool, second_pass_exit_code: Option<i32>, extra: &[&str]| {
        let mut all = warnings.clone();
        all.extend(extra.iter().map(|warning| (*warning).to_owned()));
        json!({
            "schemaVersion": 1,
            "ok": true,
            "kind": "rewrite",
            "workdir": workdir,
            "applied": applied,
            "matches": matches,
            "counts": counts,
            "truncation": truncation,
            "application": {
                "requested": input.apply,
                "performed": applied,
                "countsArePreviewBased": true,
                "idempotencyChecked": false,
                "secondPassExitCode": second_pass_exit_code,
            },
            "warnings": all,
            "durationMs": elapsed(),
        })
    };

    if !input.apply {
        return success(false, None, &[]);
    }
    if preview.truncated {
        return failure(
            "PREVIEW_TRUNCATED",
            &format!(
                "Preview exceeded maxMatches={} or was only partially salvaged; a truncated preview is never applied. Narrow paths or globs, then retry.",
                input.max_matches
            ),
            "preview",
            language,
            elapsed(),
            stderr_details(&preview.stderr),
        );
    }
    if matches.is_empty() {
        return success(false, None, &[NOTHING_TO_APPLY_WARNING]);
    }
    if let Some(hook) = hooks.on_preview_complete {
        hook();
    }

    let remaining_budget_ms = input.timeout_ms.saturating_sub(elapsed());
    if remaining_budget_ms == 0 {
        return failure(
            "TIMEOUT",
            "The tool deadline expired before the mutation pass could start; nothing was modified.",
            "apply",
            language,
            elapsed(),
            stderr_details(&preview.stderr),
        );
    }

    let applied = match spawn_sg_runner(SgRunnerInput {
        sg_path,
        args: build_rewrite_apply_args(&input),
        workdir: &workdir,
        env: None,
        max_matches: Some(input.max_matches),
        timeout_ms: Some(remaining_budget_ms),
        signal,
    }) {
        Ok(applied) => applied,
        Err(error) => {
            return failure(
                error.code.as_str(),
                &error.message,
                "apply",
                language,
                elapsed(),
                stderr_details(&error.stderr),
            );
        }
    };

    if applied.exit_code == Some(1) {
        return failure(
            "REWRITE_STALE_PREVIEW",
            "The preview found matches but the mutation pass found none; the files or search scope changed between passes. Re-run the preview before applying.",
            "apply",
            language,
            elapsed(),
            stderr_details(&applied.stderr),
        );
    }
    success(true, applied.exit_code, &[APPLY_PREVIEW_WARNING])
}
