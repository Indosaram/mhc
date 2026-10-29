use std::sync::LazyLock;
use std::time::Instant;

use fancy_regex::Regex;
use serde_json::Map;
use serde_json::Value;
use serde_json::json;

use super::MAX_GLOBS;
use super::MAX_PATHS;
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
use crate::sg_runner::SgRunnerError;
use crate::sg_runner::SgRunnerInput;
use crate::sg_runner::reason_value;
use crate::sg_runner::spawn_sg_runner;

pub const MAX_PATH_LENGTH: usize = 4_096;
pub const MAX_GLOB_LENGTH: usize = 1_024;
pub const MAX_INLINE_RULE_BYTES: usize = 64 * 1_024;

pub const SCAN_TOOL_NAME: &str = "scan";
pub const SCAN_TOOL_DESCRIPTION: &str = "Scan files with exactly one explicit ast-grep YAML rule source. Provide either ruleFile or inlineRules; ambient sgconfig.yml discovery is never used. Dry-run is the default. Apply uses a bounded JSON preview followed by a separate plain --update-all pass, and truncated previews are never applied.";
pub const APPLY_PREVIEW_WARNING: &str = "Mutation counts are based on the preview pass; sg scan --update-all does not return equivalent JSON.";

const RETRYABLE: [&str; 3] = ["ABORTED", "OUTPUT_PARSE_FAILED", "TIMEOUT"];
const KNOWN_KEYS: [&str; 11] = [
    "ruleFile",
    "inlineRules",
    "paths",
    "workdir",
    "globs",
    "maxMatches",
    "timeoutMs",
    "includeHidden",
    "followSymlinks",
    "includeMetadata",
    "apply",
];
const RULE_KEYS: [&str; 6] = [
    "ruleId", "severity", "note", "message", "labels", "metadata",
];

static RULE_PARSE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)Cannot parse rule|not a valid ast-grep rule|Fail to parse yaml as RuleConfig")
        .expect("static regex compiles")
});
static DEPRECATION_WARNING_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?im)^warning:.*\bsg\b.*deprecated").expect("static regex compiles")
});

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuleSource {
    File(String),
    Inline(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ScanInput {
    pub source: RuleSource,
    pub paths: Vec<String>,
    pub workdir: Option<String>,
    pub globs: Option<Vec<String>>,
    pub max_matches: u64,
    pub timeout_ms: u64,
    pub include_hidden: Option<bool>,
    pub follow_symlinks: Option<bool>,
    pub include_metadata: bool,
    pub apply: bool,
}

pub fn parse_scan_input(input: &Value) -> Result<ScanInput, String> {
    let obj = as_object(input)?;
    super::reject_unknown(obj, &KNOWN_KEYS)?;
    let rule_file = obj.get("ruleFile");
    let inline_rules = obj.get("inlineRules");
    if rule_file.is_some() == inline_rules.is_some() {
        return Err("Exactly one of ruleFile or inlineRules must be provided".into());
    }
    let source = if let Some(value) = rule_file {
        let path = value
            .as_str()
            .filter(|path| !path.is_empty())
            .ok_or("ruleFile must be a non-empty string")?;
        if code_points(path) > MAX_PATH_LENGTH {
            return Err(format!(
                "ruleFile must be at most {MAX_PATH_LENGTH} characters"
            ));
        }
        RuleSource::File(path.to_owned())
    } else {
        let text = inline_rules
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
            .ok_or("inlineRules must be a non-empty string")?;
        if text.len() > MAX_INLINE_RULE_BYTES {
            return Err("inlineRules must be at most 64KiB".into());
        }
        RuleSource::Inline(text.to_owned())
    };
    if !obj.contains_key("paths") {
        return Err("paths must be an array".into());
    }
    let paths =
        string_list(obj, "paths", "path", 1, MAX_PATHS, MAX_PATH_LENGTH)?.unwrap_or_default();
    let workdir = match obj.get("workdir") {
        None => None,
        Some(value) => {
            let text = value
                .as_str()
                .filter(|text| !text.is_empty())
                .ok_or("workdir must be a non-empty string")?;
            if code_points(text) > MAX_PATH_LENGTH {
                return Err(format!(
                    "workdir must be at most {MAX_PATH_LENGTH} characters"
                ));
            }
            Some(text.to_owned())
        }
    };
    let globs = string_list(obj, "globs", "glob", 0, MAX_GLOBS, MAX_GLOB_LENGTH)?;
    let max_matches = super::max_matches(obj)?;
    let timeout_ms = super::timeout_ms(obj)?;
    let include_hidden = optional_bool(obj, "includeHidden")?;
    let follow_symlinks = optional_bool(obj, "followSymlinks")?;
    let include_metadata = optional_bool(obj, "includeMetadata")?.unwrap_or(false);
    let apply = optional_bool(obj, "apply")?.unwrap_or(false);
    Ok(ScanInput {
        source,
        paths,
        workdir,
        globs,
        max_matches,
        timeout_ms,
        include_hidden,
        follow_symlinks,
        include_metadata,
        apply,
    })
}

fn source_args(input: &ScanInput) -> Vec<String> {
    match &input.source {
        RuleSource::File(path) => vec!["--rule".into(), path.clone()],
        RuleSource::Inline(text) => vec!["--inline-rules".into(), text.clone()],
    }
}

fn scan_scope_args(input: &ScanInput) -> Vec<String> {
    scope_args(
        input.globs.as_deref(),
        input.include_hidden.unwrap_or(false),
        input.follow_symlinks.unwrap_or(false),
    )
}

pub fn build_scan_args(input: &ScanInput) -> Vec<String> {
    let mut args = vec!["scan".to_owned()];
    args.extend(source_args(input));
    if input.include_metadata {
        args.push("--include-metadata".into());
    }
    args.push("--json=stream".into());
    args.extend(scan_scope_args(input));
    args.extend(input.paths.iter().cloned());
    args
}

pub fn build_scan_apply_args(input: &ScanInput) -> Vec<String> {
    let mut args = vec!["scan".to_owned()];
    args.extend(source_args(input));
    args.push("--update-all".into());
    args.extend(scan_scope_args(input));
    args.extend(input.paths.iter().cloned());
    args
}

fn failure(
    code: &str,
    message: &str,
    phase: &str,
    duration_ms: u64,
    stderr: Option<&str>,
) -> Value {
    let mut details = Map::new();
    if let Some(stderr) = stderr {
        details.insert("stderr".into(), json!(stderr));
    }
    json!({
        "schemaVersion": 1,
        "ok": false,
        "kind": "scan",
        "error": {
            "code": code,
            "message": message,
            "retryable": RETRYABLE.contains(&code),
            "phase": phase,
            "details": details,
        },
        "durationMs": duration_ms,
    })
}

fn runner_failure(error: &SgRunnerError, phase: &str) -> Value {
    if RULE_PARSE_RE.is_match(&error.stderr).unwrap_or(false) {
        return failure(
            "RULE_PARSE_FAILED",
            "ast-grep could not parse the explicit YAML rule source.",
            phase,
            error.duration_ms,
            Some(&error.stderr),
        );
    }
    failure(
        error.code.as_str(),
        &error.message,
        phase,
        error.duration_ms,
        Some(&error.stderr),
    )
}

fn to_scan_matches(
    records: &[Map<String, Value>],
    workdir: &str,
) -> Result<Vec<Map<String, Value>>, String> {
    let normalized = normalize_records(records, workdir).map_err(|error| error.0)?;
    Ok(normalized
        .into_iter()
        .map(|raw| {
            let mut rule = Map::new();
            rule.insert(
                "ruleId".into(),
                json!(
                    raw.get("ruleId")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                ),
            );
            for key in ["severity", "note", "message"] {
                if let Some(text) = raw.get(key).and_then(Value::as_str) {
                    rule.insert(key.into(), json!(text));
                }
            }
            let labels = raw
                .get("labels")
                .filter(|labels| labels.is_array())
                .cloned()
                .unwrap_or_else(|| json!([]));
            rule.insert("labels".into(), labels);
            if let Some(metadata) = raw.get("metadata") {
                rule.insert("metadata".into(), metadata.clone());
            }
            let replacement = raw
                .get("replacement")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let mut out: Map<String, Value> = raw
                .into_iter()
                .filter(|(key, _)| !RULE_KEYS.contains(&key.as_str()))
                .collect();
            out.insert("replacement".into(), json!(replacement));
            out.insert("rule".into(), Value::Object(rule));
            out
        })
        .collect())
}

pub fn execute_scan(raw_input: &Value, sg_path: &str, signal: Option<&AbortSignal>) -> Value {
    let started_at = Instant::now();
    let elapsed = || elapsed_ms(started_at);
    let input = match parse_scan_input(raw_input) {
        Ok(input) => input,
        Err(message) => return failure("INVALID_ARGUMENT", &message, "preflight", elapsed(), None),
    };
    let workdir = input.workdir.clone().unwrap_or_else(default_workdir);

    let preview = match spawn_sg_runner(SgRunnerInput {
        sg_path,
        args: build_scan_args(&input),
        workdir: &workdir,
        env: None,
        max_matches: Some(input.max_matches),
        timeout_ms: Some(input.timeout_ms),
        signal,
    }) {
        Ok(preview) => preview,
        Err(error) => return runner_failure(&error, "preview"),
    };

    let matches = match to_scan_matches(&preview.records, &workdir) {
        Ok(matches) => matches,
        Err(message) => return failure("SG_FAILED", &message, "preview", elapsed(), None),
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
    let warnings: Vec<String> = if DEPRECATION_WARNING_RE
        .is_match(&preview.stderr)
        .unwrap_or(false)
    {
        vec![preview.stderr.trim().to_owned()]
    } else {
        Vec::new()
    };

    let success = |applied: bool, second_pass_exit_code: Option<i32>, extra: &[&str]| {
        let mut all = warnings.clone();
        all.extend(extra.iter().map(|warning| (*warning).to_owned()));
        json!({
            "schemaVersion": 1,
            "ok": true,
            "kind": "scan",
            "workdir": workdir,
            "applied": applied,
            "matches": matches,
            "counts": counts,
            "truncation": truncation,
            "application": {
                "requested": input.apply,
                "performed": applied,
                "countsArePreviewBased": true,
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
            elapsed(),
            Some(&preview.stderr),
        );
    }
    if matches.is_empty() {
        return success(false, None, &[super::rewrite::NOTHING_TO_APPLY_WARNING]);
    }
    let remaining_budget_ms = input.timeout_ms.saturating_sub(elapsed());
    if remaining_budget_ms == 0 {
        return failure(
            "TIMEOUT",
            "The tool deadline expired during the preview pass; the mutation pass was not started.",
            "preview",
            elapsed(),
            Some(&preview.stderr),
        );
    }
    let applied = match spawn_sg_runner(SgRunnerInput {
        sg_path,
        args: build_scan_apply_args(&input),
        workdir: &workdir,
        env: None,
        max_matches: Some(input.max_matches),
        timeout_ms: Some(remaining_budget_ms),
        signal,
    }) {
        Ok(applied) => applied,
        Err(error) => return runner_failure(&error, "apply"),
    };
    success(true, applied.exit_code, &[APPLY_PREVIEW_WARNING])
}
