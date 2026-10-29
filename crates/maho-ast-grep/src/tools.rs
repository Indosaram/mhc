//! The three tools: argument handling, program invocation, result shaping (spec §5, §6).

use crate::error::{
    PATH_NOT_FOUND, PATH_UNREADABLE, PREVIEW_TRUNCATED, PATTERN_PARSE_FAILED, Phase,
    REWRITE_STALE_PREVIEW, RULE_PARSE_FAILED, SG_FAILED, TIMEOUT, ToolError,
};
use crate::exec::{
    AbortSignal, Collected, MAX_PAYLOAD_BYTES, RunSpec, Truncation, collect, finish,
};
use crate::hints;
use crate::params::{Kind, Request, RuleSource, parse};
use crate::paths::relative_to;
use crate::resolve::{Toolchain, resolve};
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// What the tools need from the host process.
#[derive(Clone, Debug)]
pub struct Context {
    pub toolchain: Toolchain,
    /// Default `workdir` (spec §1); the server's own cwd when `None`.
    pub project_cwd: Option<PathBuf>,
}

/// A finished tool call: the result object and whether it is a failure.
#[derive(Debug)]
pub struct Outcome {
    pub value: Value,
    pub is_error: bool,
}

impl Outcome {
    fn failure(error: &ToolError, kind: Option<&str>, started: Instant) -> Self {
        Self {
            value: error.to_json(kind, Some(elapsed_ms(started))),
            is_error: true,
        }
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Run tool `name` with `arguments`.
pub async fn call(
    context: &Context,
    name: &str,
    arguments: Option<&Value>,
    abort: &mut AbortSignal,
) -> Outcome {
    let started = Instant::now();
    let Some(kind) = Kind::from_name(name) else {
        let error = ToolError::invalid(format!(
            "Unknown tool `{name}`. Available tools: search, rewrite, scan."
        ))
        .with_details(json!({ "tools": ["search", "rewrite", "scan"] }));
        return Outcome::failure(&error, None, started);
    };
    match run(context, kind, arguments, abort, started).await {
        Ok(value) => Outcome {
            value,
            is_error: false,
        },
        Err(error) => Outcome::failure(&error, Some(kind.as_str()), started),
    }
}

async fn run(
    context: &Context,
    kind: Kind,
    arguments: Option<&Value>,
    abort: &mut AbortSignal,
    started: Instant,
) -> Result<Value, ToolError> {
    let request = parse(kind, arguments)?;
    let mut warnings = Vec::new();
    if kind != Kind::Scan {
        warnings = hints::preflight(
            &request.pattern,
            &request.language,
            request.rewrite.as_deref(),
            request.force,
        )?;
    }
    let workdir = effective_workdir(context, &request)?;
    check_paths(&workdir, &request)?;
    let program = resolve(&context.toolchain).await?;
    let deadline = tokio::time::Instant::now() + Duration::from_millis(request.timeout_ms);

    let phase = if kind == Kind::Search {
        Phase::Search
    } else {
        Phase::Preview
    };
    let args = build_args(&request, true);
    let collected = collect(
        &RunSpec {
            program: &program,
            args: &args,
            workdir: &workdir,
            deadline,
            phase,
            max_records: request.max_matches,
        },
        abort,
    )
    .await?;
    check_exit(&request, &collected, phase)?;
    if kind == Kind::Scan {
        for line in collected.stderr.lines() {
            if line.to_ascii_lowercase().contains("deprecat") {
                warnings.push(line.trim().to_owned());
            }
        }
    }

    let workdir_text = workdir.to_string_lossy().into_owned();
    let mut matches: Vec<Value> = collected
        .records
        .iter()
        .map(|record| shape_match(kind, &workdir_text, &request.language, record))
        .collect();
    matches.sort_by(|a, b| {
        let key = |v: &Value| {
            (
                v["path"].as_str().unwrap_or_default().to_owned(),
                v["range"]["start"]["byteOffset"].as_u64().unwrap_or(0),
            )
        };
        key(a).cmp(&key(b))
    });
    let files = distinct_files(&matches);
    let truncation = truncation_json(&collected, request.max_matches);

    if kind == Kind::Search {
        let truncated = collected.truncation.is_some();
        let limited = matches!(
            collected.truncation,
            Some(Truncation::MatchLimit | Truncation::OutputCap)
        );
        if truncated {
            warnings.push(format!(
                "The result was truncated ({}); narrow `paths`, `globs` or the pattern, or lower the scope, to see every match.",
                collected.truncation.map_or("", Truncation::as_str)
            ));
        }
        let count = matches.len();
        return Ok(json!({
            "schemaVersion": 1,
            "ok": true,
            "kind": "search",
            "workdir": workdir_text,
            "counts": {
                "returnedMatches": count,
                "returnedFiles": files,
                "totalMatches": if truncated { Value::Null } else { json!(count) },
                "totalFiles": if truncated { Value::Null } else { json!(files) },
                "atLeastMatches": if limited { count + 1 } else { count },
            },
            "matches": matches,
            "truncation": truncation,
            "warnings": warnings,
            "durationMs": elapsed_ms(started),
        }));
    }

    // rewrite / scan
    let mut applied = false;
    let mut second_exit = Value::Null;
    if request.apply {
        if collected.truncation.is_some() {
            return Err(ToolError::new(
                PREVIEW_TRUNCATED,
                Phase::Apply,
                "The preview was truncated, so applying it could change files the caller has not seen. Narrow the scope and retry.",
            )
            .with_details(json!({ "truncation": truncation })));
        }
        if matches.is_empty() {
            warnings.push("The preview found no matches; nothing was applied.".to_owned());
        } else {
            let now = tokio::time::Instant::now();
            if now >= deadline {
                return Err(ToolError::new(
                    TIMEOUT,
                    Phase::Apply,
                    "No time was left for the apply pass.",
                ));
            }
            let apply_args = build_args(&request, false);
            let done = finish(
                &RunSpec {
                    program: &program,
                    args: &apply_args,
                    workdir: &workdir,
                    deadline,
                    phase: Phase::Apply,
                    max_records: 0,
                },
                abort,
            )
            .await?;
            if done.exit_code == 1 && kind == Kind::Rewrite {
                return Err(ToolError::new(
                    REWRITE_STALE_PREVIEW,
                    Phase::Apply,
                    "The apply pass found nothing to change; the files changed after the preview. Run the preview again.",
                ));
            }
            if done.exit_code != 0 && done.exit_code != 1 {
                return Err(sg_failed(Phase::Apply, done.exit_code, &done.stderr));
            }
            applied = true;
            second_exit = json!(done.exit_code);
            warnings.push(
                "The counts and matches are from the preview pass, not re-read after applying."
                    .to_owned(),
            );
        }
    }
    let mut application = json!({
        "requested": request.apply,
        "performed": applied,
        "countsArePreviewBased": true,
        "secondPassExitCode": second_exit,
    });
    if kind == Kind::Rewrite {
        application["idempotencyChecked"] = json!(false);
    }
    Ok(json!({
        "schemaVersion": 1,
        "ok": true,
        "kind": kind.as_str(),
        "workdir": workdir_text,
        "applied": applied,
        "matches": matches,
        "counts": { "plannedMatches": collected.records.len(), "plannedFiles": files },
        "truncation": truncation,
        "application": application,
        "warnings": warnings,
        "durationMs": elapsed_ms(started),
    }))
}

fn effective_workdir(context: &Context, request: &Request) -> Result<PathBuf, ToolError> {
    let chosen = match (&request.workdir, &context.project_cwd) {
        (Some(dir), _) => PathBuf::from(dir),
        (None, Some(dir)) => dir.clone(),
        (None, None) => std::env::current_dir().map_err(|error| {
            ToolError::new(PATH_UNREADABLE, Phase::Preflight, format!("No working directory: {error}"))
        })?,
    };
    match std::fs::metadata(&chosen) {
        Ok(meta) if meta.is_dir() => Ok(chosen),
        Ok(_) => Err(ToolError::new(
            PATH_NOT_FOUND,
            Phase::Preflight,
            format!("`workdir` is not a directory: {}", chosen.display()),
        )),
        Err(error) => Err(path_error(&chosen, &error)),
    }
}

fn path_error(path: &Path, error: &std::io::Error) -> ToolError {
    let code = if error.kind() == std::io::ErrorKind::NotFound {
        PATH_NOT_FOUND
    } else {
        PATH_UNREADABLE
    };
    ToolError::new(
        code,
        Phase::Preflight,
        format!("Cannot use path {}: {error}", path.display()),
    )
    .with_details(json!({ "path": path.to_string_lossy() }))
}

fn check_paths(workdir: &Path, request: &Request) -> Result<(), ToolError> {
    for path in &request.paths {
        let full = workdir.join(path);
        if let Err(error) = std::fs::metadata(&full) {
            return Err(path_error(&full, &error));
        }
    }
    if let Some(RuleSource::File(file)) = &request.rule {
        let full = workdir.join(file);
        if let Err(error) = std::fs::metadata(&full) {
            return Err(path_error(&full, &error));
        }
    }
    Ok(())
}

/// Program arguments; `preview` selects `--json=stream`, otherwise `--update-all`.
pub fn build_args(request: &Request, preview: bool) -> Vec<String> {
    let mut args: Vec<String> = Vec::new();
    let push = |args: &mut Vec<String>, items: &[&str]| {
        args.extend(items.iter().map(|item| (*item).to_owned()));
    };
    match request.kind {
        Kind::Scan => {
            push(&mut args, &["scan"]);
            // Only the given rule source counts: point the program at an empty project config so
            // an `sgconfig.yml` in the tree cannot change the result.
            if let Some(config) = empty_config() {
                push(&mut args, &["--config", &config]);
            }
            match &request.rule {
                Some(RuleSource::File(file)) => push(&mut args, &["--rule", file]),
                Some(RuleSource::Inline(text)) => push(&mut args, &["--inline-rules", text]),
                None => {}
            }
            if request.include_metadata {
                push(&mut args, &["--include-metadata"]);
            }
        }
        Kind::Search | Kind::Rewrite => {
            push(
                &mut args,
                &[
                    "run",
                    "-p",
                    &request.pattern,
                    "--lang",
                    &request.language,
                    "--strictness",
                    &request.strictness,
                ],
            );
            if let Some(selector) = &request.selector {
                push(&mut args, &["--selector", selector]);
            }
            if let Some(rewrite) = &request.rewrite {
                push(&mut args, &["-r", rewrite]);
            }
        }
    }
    push(
        &mut args,
        &[if preview { "--json=stream" } else { "--update-all" }],
    );
    for glob in &request.globs {
        push(&mut args, &["--globs", glob]);
    }
    if request.include_hidden {
        push(&mut args, &["--no-ignore", "hidden"]);
    }
    if request.follow_symlinks {
        push(&mut args, &["--follow"]);
    }
    // `--` keeps a path that starts with a dash from being read as an option.
    push(&mut args, &["--"]);
    args.extend(request.paths.iter().cloned());
    args
}

/// Path of an empty project config file, written once per process.
fn empty_config() -> Option<String> {
    static PATH: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    PATH.get_or_init(|| {
        let path = std::env::temp_dir().join(format!("mhc-ast-grep-{}-empty.yml", std::process::id()));
        std::fs::write(&path, "ruleDirs: []\n").ok()?;
        Some(path.to_string_lossy().into_owned())
    })
    .clone()
}

fn sg_failed(phase: Phase, exit_code: i32, stderr: &str) -> ToolError {
    ToolError::new(
        SG_FAILED,
        phase,
        format!("ast-grep exited with code {exit_code}: {}", first_line(stderr)),
    )
    .with_details(json!({ "exitCode": exit_code, "stderr": stderr }))
}

fn first_line(text: &str) -> &str {
    text.lines().find(|line| !line.trim().is_empty()).unwrap_or("").trim()
}

fn stderr_has_error_line(stderr: &str) -> bool {
    stderr.lines().any(|line| {
        let line = line.trim_start();
        line.starts_with("ERROR") || line.starts_with("error:")
    })
}

fn check_exit(request: &Request, collected: &Collected, phase: Phase) -> Result<(), ToolError> {
    let stderr = &collected.stderr;
    if request.kind == Kind::Scan {
        let lower = stderr.to_ascii_lowercase();
        if lower.contains("cannot parse rule")
            || lower.contains("fail to parse yaml")
            || lower.contains("cannot read rule")
        {
            return Err(ToolError::new(
                RULE_PARSE_FAILED,
                phase,
                format!("The rule could not be parsed: {}", first_line(stderr)),
            )
            .with_details(json!({ "stderr": stderr })));
        }
    } else if stderr.to_ascii_lowercase().contains("error node") || stderr.contains("Cannot parse query") {
        return Err(ToolError::new(
            PATTERN_PARSE_FAILED,
            phase,
            "The pattern does not parse as code in this language (it contains an error node).",
        )
        .with_language(&request.language)
        .with_details(json!({ "stderr": stderr })));
    }
    let early = collected.truncation.is_some_and(|t| t != Truncation::SgOutputTruncated);
    if early {
        return Ok(());
    }
    let code = collected.exit_code;
    if (code != 0 && code != 1) || (code == 1 && stderr_has_error_line(stderr)) {
        return Err(sg_failed(phase, code, stderr));
    }
    Ok(())
}

fn distinct_files(matches: &[Value]) -> usize {
    let mut seen: Vec<&str> = matches.iter().filter_map(|m| m["path"].as_str()).collect();
    seen.sort_unstable();
    seen.dedup();
    seen.len()
}

fn truncation_json(collected: &Collected, max_matches: usize) -> Value {
    json!({
        "truncated": collected.truncation.is_some(),
        "reason": collected.truncation.map(Truncation::as_str),
        "maxMatches": max_matches,
        "maxPayloadBytes": MAX_PAYLOAD_BYTES,
        "salvagedRecords": collected.salvaged,
    })
}

fn position(value: &Value, offset: &Value) -> Value {
    json!({
        "line": value["line"].as_u64().unwrap_or(0) + 1,
        "column": value["column"].as_u64().unwrap_or(0),
        "byteOffset": offset.as_u64().unwrap_or(0),
    })
}

fn multi_text(nodes: &[Value], match_text: &str, match_start: u64) -> String {
    let (Some(first), Some(last)) = (nodes.first(), nodes.last()) else {
        return String::new();
    };
    let start = first["range"]["byteOffset"]["start"].as_u64();
    let end = last["range"]["byteOffset"]["end"].as_u64();
    if let (Some(start), Some(end)) = (start, end)
        && start >= match_start
        && end >= start
    {
        let from = usize::try_from(start - match_start).unwrap_or(usize::MAX);
        let to = usize::try_from(end - match_start).unwrap_or(usize::MAX);
        if let Some(slice) = match_text.as_bytes().get(from..to)
            && let Ok(text) = std::str::from_utf8(slice)
        {
            return text.to_owned();
        }
    }
    nodes
        .iter()
        .filter_map(|node| node["text"].as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

fn shape_match(kind: Kind, workdir: &str, language: &str, record: &Map<String, Value>) -> Value {
    let get = |key: &str| record.get(key).cloned().unwrap_or(Value::Null);
    let file = record.get("file").and_then(Value::as_str).unwrap_or_default();
    let full = if crate::paths::is_absolute(&file.replace('\\', "/")) {
        file.to_owned()
    } else {
        format!("{}/{}", workdir.trim_end_matches(['/', '\\']), file)
    };
    let path = relative_to(workdir, &full);
    let range = get("range");
    let offsets = &range["byteOffset"];
    let match_text = record.get("text").and_then(Value::as_str).unwrap_or_default();
    let match_start = offsets["start"].as_u64().unwrap_or(0);

    let mut single = Map::new();
    let mut multi = Map::new();
    let meta = &record.get("metaVariables").cloned().unwrap_or(Value::Null);
    if let Some(items) = meta["single"].as_object() {
        for (name, node) in items {
            single.insert(name.clone(), json!(node["text"].as_str().unwrap_or_default()));
        }
    }
    if let Some(items) = meta["multi"].as_object() {
        for (name, nodes) in items {
            let nodes = nodes.as_array().map_or(&[][..], Vec::as_slice);
            multi.insert(name.clone(), json!(multi_text(nodes, match_text, match_start)));
        }
    }

    let mut out = Map::new();
    out.insert("path".into(), json!(path));
    out.insert(
        "language".into(),
        json!(
            record
                .get("language")
                .and_then(Value::as_str)
                .map_or_else(|| language.to_owned(), str::to_ascii_lowercase)
        ),
    );
    out.insert("text".into(), json!(match_text));
    out.insert(
        "range".into(),
        json!({
            "start": position(&range["start"], &offsets["start"]),
            "end": position(&range["end"], &offsets["end"]),
        }),
    );
    out.insert(
        "metavariables".into(),
        json!({ "single": single, "multi": multi }),
    );
    let scan_keys = ["ruleId", "severity", "note", "message", "labels", "metadata"];
    for (key, value) in record {
        let consumed = matches!(
            key.as_str(),
            "file" | "range" | "language" | "text" | "metaVariables" | "replacement"
        ) || (kind == Kind::Scan && scan_keys.contains(&key.as_str()));
        if !consumed {
            out.insert(key.clone(), value.clone());
        }
    }
    if kind != Kind::Search {
        out.insert(
            "replacement".into(),
            json!(record.get("replacement").and_then(Value::as_str).unwrap_or_default()),
        );
    }
    if kind == Kind::Scan {
        let mut rule = Map::new();
        for key in scan_keys {
            match record.get(key) {
                Some(value) => {
                    rule.insert(key.to_owned(), value.clone());
                }
                None if key == "labels" => {
                    rule.insert(key.to_owned(), json!([]));
                }
                None => {}
            }
        }
        out.insert("rule".into(), Value::Object(rule));
    }
    Value::Object(out)
}
