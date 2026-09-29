mod common;

use std::path::Path;
use std::path::PathBuf;

use ast_grep_mcp::tools::rewrite::REWRITE_TOOL_DESCRIPTION;
use ast_grep_mcp::tools::rewrite::REWRITE_TOOL_NAME;
use ast_grep_mcp::tools::rewrite::RewriteHooks;
use ast_grep_mcp::tools::rewrite::build_rewrite_apply_args;
use ast_grep_mcp::tools::rewrite::build_rewrite_args;
use ast_grep_mcp::tools::rewrite::execute_rewrite;
use ast_grep_mcp::tools::rewrite::parse_rewrite_input;
use common::SG_PATH;
use common::path_str;
use common::sg_available;
use pretty_assertions::assert_eq;
use serde_json::Map;
use serde_json::Value;
use serde_json::json;

const ASTRAL: &str = "\u{1D11E}";

fn fixture(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::Builder::new()
        .prefix("omo-rewrite-")
        .tempdir()
        .unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    for (relative, body) in files {
        let target = dir.path().join(relative);
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(target, body).unwrap();
    }
    dir
}

fn default_fixture() -> tempfile::TempDir {
    fixture(&[
        (
            "src/a.ts",
            "console.log(\"hello\");\nconsole.log(\"world\");\nconst x = 1;\nconsole.log(x);\n",
        ),
        ("src/b.ts", "console.log(\"from-b\");\n"),
    ])
}

fn target(dir: &tempfile::TempDir) -> PathBuf {
    dir.path().join("src/a.ts")
}

fn contents(path: &Path) -> Vec<u8> {
    std::fs::read(path).unwrap()
}

fn read_text(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap()
}

fn base_input(dir: &Path, overrides: &[(&str, Value)]) -> Value {
    let mut input = json!({
        "pattern": "console.log($MSG)",
        "rewrite": "logger.info($MSG)",
        "language": "typescript",
        "paths": [path_str(&dir.join("src"))],
        "workdir": path_str(dir),
        "strictness": "smart",
        "maxMatches": 50,
        "timeoutMs": 60_000,
        "apply": false,
    })
    .as_object()
    .cloned()
    .unwrap();
    for (key, value) in overrides {
        input.insert((*key).to_owned(), value.clone());
    }
    Value::Object(input)
}

fn schema_base(overrides: &[(&str, Value)]) -> Value {
    let mut input: Map<String, Value> = json!({
        "pattern": "console.log($MSG)",
        "rewrite": "logger.info($MSG)",
        "language": "typescript",
        "paths": ["src"],
    })
    .as_object()
    .cloned()
    .unwrap();
    for (key, value) in overrides {
        input.insert((*key).to_owned(), value.clone());
    }
    Value::Object(input)
}

fn run(input: &Value) -> Value {
    execute_rewrite(input, SG_PATH, None, &RewriteHooks::default())
}

#[test]
fn exports_raw_tool_name() {
    assert_eq!(REWRITE_TOOL_NAME, "rewrite");
}

#[test]
fn description_is_verbatim() {
    assert_eq!(
        REWRITE_TOOL_DESCRIPTION,
        "Preview or apply an AST-aware rewrite. The pattern follows the same metavariable rules as `search`; the replacement may only reference metavariables captured by the pattern, and an empty replacement deletes the match. Dry-run is the default. Apply uses a JSON preview followed by a separate `--update-all` process because `sg` cannot safely combine JSON output and mutation. Truncated previews are never applied, and rewrite idempotency is not guaranteed."
    );
}

#[test]
fn schema_defaults() {
    let parsed = parse_rewrite_input(&schema_base(&[])).unwrap();
    assert!(!parsed.apply);
    assert_eq!(parsed.strictness, "smart");
    assert_eq!(parsed.max_matches, 50);
    assert_eq!(parsed.timeout_ms, 300_000);
}

#[test]
fn schema_accepts_empty_rewrite_as_deletion() {
    let parsed = parse_rewrite_input(&schema_base(&[("rewrite", json!(""))])).unwrap();
    assert_eq!(parsed.rewrite, "");
}

#[test]
fn schema_rejects_missing_rewrite() {
    let mut input = schema_base(&[]);
    input.as_object_mut().unwrap().remove("rewrite");
    assert!(parse_rewrite_input(&input).is_err());
}

#[test]
fn schema_rejects_rewrite_over_64kib() {
    assert!(parse_rewrite_input(&schema_base(&[("rewrite", json!("y".repeat(65_537)))])).is_err());
}

#[test]
fn schema_rejects_pattern_over_16kib() {
    assert!(
        parse_rewrite_input(&schema_base(&[
            ("pattern", json!("x".repeat(16_385))),
            ("rewrite", json!("y"))
        ]))
        .is_err()
    );
}

#[test]
fn schema_rejects_more_than_64_paths() {
    let paths: Vec<String> = (0..65).map(|index| format!("src/{index}")).collect();
    assert!(parse_rewrite_input(&schema_base(&[("paths", json!(paths))])).is_err());
}

#[test]
fn schema_rejects_unknown_property() {
    assert!(parse_rewrite_input(&schema_base(&[("updateAll", json!(true))])).is_err());
}

#[test]
fn schema_rejects_non_boolean_apply() {
    assert!(parse_rewrite_input(&schema_base(&[("apply", json!("yes"))])).is_err());
}

#[test]
fn schema_rejects_max_matches_above_500() {
    assert!(parse_rewrite_input(&schema_base(&[("maxMatches", json!(501))])).is_err());
}

fn oversized() -> Vec<(&'static str, Value)> {
    vec![
        ("paths", json!(["x".repeat(4097)])),
        ("workdir", json!("x".repeat(4097))),
        ("globs", json!(["x".repeat(1025)])),
        ("selector", json!("x".repeat(129))),
    ]
}

#[test]
fn per_string_bounds_reject_when_parsed() {
    for (key, value) in oversized() {
        assert!(
            parse_rewrite_input(&schema_base(&[(key, value)])).is_err(),
            "oversized {key} must reject"
        );
    }
}

#[test]
fn per_string_bounds_reject_before_spawn_when_executed() {
    for (key, value) in oversized() {
        let dir = default_fixture();
        let before = contents(&target(&dir));
        let mut input = base_input(dir.path(), &[("apply", json!(true))]);
        input.as_object_mut().unwrap().insert(key.to_owned(), value);
        let result = run(&input);
        assert_eq!(result["ok"], json!(false), "{key}");
        assert_eq!(result["error"]["code"], json!("INVALID_ARGUMENT"), "{key}");
        assert_eq!(result["error"]["phase"], json!("preflight"), "{key}");
        assert_eq!(contents(&target(&dir)), before, "{key}");
    }
}

#[test]
fn per_string_bounds_accept_exact_limits() {
    assert!(
        parse_rewrite_input(&schema_base(&[
            ("paths", json!(["x".repeat(4096)])),
            ("workdir", json!("y".repeat(4096))),
            ("globs", json!(["z".repeat(1024)])),
            ("selector", json!("s".repeat(128))),
        ]))
        .is_ok()
    );
}

type AstralField = (&'static str, usize, fn(String) -> (&'static str, Value));

fn astral_fields() -> Vec<AstralField> {
    vec![
        ("path", 4096, |value| ("paths", json!([value]))),
        ("workdir", 4096, |value| ("workdir", json!(value))),
        ("glob", 1024, |value| ("globs", json!([value]))),
        ("selector", 128, |value| ("selector", json!(value))),
    ]
}

fn parse_with(entry: (&str, Value)) -> Result<(), String> {
    parse_rewrite_input(&schema_base(&[entry])).map(|_| ())
}

#[test]
fn astral_character_is_one_code_point_but_two_utf16_units() {
    assert_eq!(ASTRAL.chars().count(), 1);
    assert_eq!(ASTRAL.encode_utf16().count(), 2);
}

#[test]
fn astral_exact_limit_is_accepted() {
    for (label, limit, build) in astral_fields() {
        let value = ASTRAL.repeat(limit);
        assert_eq!(value.chars().count(), limit);
        assert_eq!(value.encode_utf16().count(), limit * 2);
        assert!(parse_with(build(value)).is_ok(), "{label}");
    }
}

#[test]
fn astral_limit_plus_one_rejects() {
    for (label, limit, build) in astral_fields() {
        assert!(
            parse_with(build(ASTRAL.repeat(limit + 1))).is_err(),
            "{label}"
        );
    }
}

#[test]
fn astral_mixed_straddling_limit_only_limit_plus_one_rejects() {
    for (label, limit, build) in astral_fields() {
        let at_limit = format!("{}{}", ASTRAL.repeat(limit / 2), "a".repeat(limit / 2));
        let over_limit = format!("{at_limit}a");
        assert_eq!(at_limit.chars().count(), limit);
        assert_eq!(over_limit.chars().count(), limit + 1);
        assert!(parse_with(build(at_limit)).is_ok(), "{label}");
        assert!(parse_with(build(over_limit)).is_err(), "{label}");
    }
}

#[test]
fn astral_over_limit_rejects_before_spawn_when_executed() {
    for (label, limit, build) in astral_fields() {
        let dir = default_fixture();
        let before = contents(&target(&dir));
        let (key, value) = build(ASTRAL.repeat(limit + 1));
        let mut input = base_input(dir.path(), &[("apply", json!(true))]);
        input.as_object_mut().unwrap().insert(key.to_owned(), value);
        let result = run(&input);
        assert_eq!(result["ok"], json!(false), "{label}");
        assert_eq!(
            result["error"]["code"],
            json!("INVALID_ARGUMENT"),
            "{label}"
        );
        assert_eq!(contents(&target(&dir)), before, "{label}");
    }
}

fn parsed(overrides: &[(&str, Value)]) -> ast_grep_mcp::tools::rewrite::RewriteInput {
    parse_rewrite_input(&base_input(Path::new("/tmp/x"), overrides)).unwrap()
}

#[test]
fn cli_preview_emits_json_stream_and_never_update_all() {
    let args = build_rewrite_args(&parsed(&[]));
    assert_eq!(
        args[..7],
        [
            "run",
            "-p",
            "console.log($MSG)",
            "-r",
            "logger.info($MSG)",
            "--lang",
            "typescript"
        ]
    );
    assert!(args.contains(&"--json=stream".to_owned()));
    assert!(!args.contains(&"--update-all".to_owned()));
    assert!(!args.contains(&"-C".to_owned()));
}

#[test]
fn cli_apply_emits_update_all_and_never_json_stream() {
    let args = build_rewrite_apply_args(&parsed(&[("apply", json!(true))]));
    assert!(args.contains(&"--update-all".to_owned()));
    assert!(!args.contains(&"--json=stream".to_owned()));
    assert!(!args.contains(&"-C".to_owned()));
}

#[test]
fn cli_both_passes_share_scope_flags() {
    let input = parsed(&[
        ("globs", json!(["*.ts", "!*.test.ts"])),
        ("includeHidden", json!(true)),
        ("followSymlinks", json!(true)),
        ("selector", json!("call_expression")),
    ]);
    let preview: Vec<String> = build_rewrite_args(&input)
        .into_iter()
        .filter(|arg| arg != "--json=stream")
        .collect();
    let apply: Vec<String> = build_rewrite_apply_args(&input)
        .into_iter()
        .filter(|arg| arg != "--update-all")
        .collect();
    assert_eq!(preview, apply);
    for expected in [
        "--globs",
        "!*.test.ts",
        "--no-ignore",
        "hidden",
        "--follow",
        "--selector",
    ] {
        assert!(preview.contains(&expected.to_owned()), "{expected}");
    }
}

#[test]
fn preflight_unbound_metavariable_with_force() {
    let dir = default_fixture();
    let before = contents(&target(&dir));
    let result = run(&base_input(
        dir.path(),
        &[
            ("rewrite", json!("logger.info($OTHER)")),
            ("apply", json!(true)),
            ("force", json!(true)),
        ],
    ));
    assert_eq!(result["ok"], json!(false));
    assert_eq!(
        result["error"]["code"],
        json!("REWRITE_UNBOUND_METAVARIABLE")
    );
    assert_eq!(contents(&target(&dir)), before);
}

#[test]
fn preflight_cardinality_mismatch_with_force() {
    let dir = default_fixture();
    let before = contents(&target(&dir));
    let result = run(&base_input(
        dir.path(),
        &[
            ("pattern", json!("console.log($ARGS)")),
            ("rewrite", json!("logger.info($$$ARGS)")),
            ("apply", json!(true)),
            ("force", json!(true)),
        ],
    ));
    assert_eq!(result["ok"], json!(false));
    assert_eq!(
        result["error"]["code"],
        json!("REWRITE_METAVARIABLE_KIND_MISMATCH")
    );
    assert_eq!(contents(&target(&dir)), before);
}

#[test]
fn preflight_regex_misuse_without_force_is_rejected() {
    let dir = default_fixture();
    let result = run(&base_input(
        dir.path(),
        &[
            ("pattern", json!("console.log(.*)")),
            ("rewrite", json!("logger.info()")),
        ],
    ));
    assert_eq!(result["ok"], json!(false));
    assert_eq!(result["error"]["code"], json!("PATTERN_HINT_REJECTED"));
}

#[test]
fn preflight_regex_misuse_with_force_bypasses_heuristic() {
    let dir = default_fixture();
    let result = run(&base_input(
        dir.path(),
        &[
            ("pattern", json!("console.log(.*)")),
            ("rewrite", json!("logger.info()")),
            ("force", json!(true)),
        ],
    ));
    if result["ok"] != json!(true) {
        assert_ne!(result["error"]["code"], json!("PATTERN_HINT_REJECTED"));
    }
}

#[test]
fn dry_run_previews_and_never_mutates() {
    if !sg_available() {
        return;
    }
    let dir = default_fixture();
    let before = contents(&target(&dir));
    let payload = run(&json!({
        "pattern": "console.log($MSG)",
        "rewrite": "logger.info($MSG)",
        "language": "typescript",
        "paths": [path_str(&dir.path().join("src"))],
        "workdir": path_str(dir.path()),
    }));
    assert_eq!(payload["ok"], json!(true));
    assert_eq!(payload["schemaVersion"], json!(1));
    assert_eq!(payload["kind"], json!("rewrite"));
    assert_eq!(payload["applied"], json!(false));
    assert_eq!(payload["matches"].as_array().unwrap().len(), 4);
    assert_eq!(
        payload["matches"][0]["replacement"],
        json!("logger.info(\"hello\")")
    );
    assert_eq!(
        payload["counts"],
        json!({ "plannedMatches": 4, "plannedFiles": 2 })
    );
    assert_eq!(
        payload["application"],
        json!({ "requested": false, "performed": false, "countsArePreviewBased": true, "idempotencyChecked": false, "secondPassExitCode": null })
    );
    assert_eq!(contents(&target(&dir)), before);
}

#[test]
fn dry_run_empty_rewrite_previews_deletion() {
    if !sg_available() {
        return;
    }
    let dir = fixture(&[("src/a.ts", "console.log(\"hello\");\n")]);
    let before = contents(&target(&dir));
    let payload = run(&base_input(dir.path(), &[("rewrite", json!(""))]));
    assert_eq!(payload["ok"], json!(true));
    assert_eq!(payload["matches"][0]["replacement"], json!(""));
    assert_eq!(contents(&target(&dir)), before);
}

#[test]
fn apply_empty_rewrite_deletes_matched_node() {
    if !sg_available() {
        return;
    }
    let dir = fixture(&[("src/a.ts", "console.log(\"hello\");\nconst x = 1;\n")]);
    let payload = run(&base_input(
        dir.path(),
        &[("rewrite", json!("")), ("apply", json!(true))],
    ));
    assert_eq!(payload["ok"], json!(true));
    assert_eq!(payload["applied"], json!(true));
    assert_eq!(read_text(&target(&dir)), ";\nconst x = 1;\n");
}

#[test]
fn dry_run_multi_metavariable_expands_captured_nodes() {
    if !sg_available() {
        return;
    }
    let dir = fixture(&[("src/a.ts", "foo(1, 2, 3);\n")]);
    let payload = run(&base_input(
        dir.path(),
        &[
            ("pattern", json!("foo($$$ARGS)")),
            ("rewrite", json!("bar($$$ARGS)")),
        ],
    ));
    assert_eq!(payload["ok"], json!(true));
    assert_eq!(payload["matches"][0]["replacement"], json!("bar(1, 2, 3)"));
    assert_eq!(
        payload["matches"][0]["metavariables"]["multi"]["ARGS"],
        json!("1, 2, 3")
    );
}

#[test]
fn apply_clean_preview_mutates_files() {
    if !sg_available() {
        return;
    }
    let dir = default_fixture();
    let before = contents(&target(&dir));
    let payload = run(&base_input(dir.path(), &[("apply", json!(true))]));
    assert_eq!(payload["ok"], json!(true));
    assert_eq!(payload["applied"], json!(true));
    assert_eq!(
        payload["application"],
        json!({ "requested": true, "performed": true, "countsArePreviewBased": true, "idempotencyChecked": false, "secondPassExitCode": 0 })
    );
    assert!(
        payload["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning.as_str().unwrap().contains("preview"))
    );
    assert_ne!(contents(&target(&dir)), before);
    let text = read_text(&target(&dir));
    assert!(text.contains("logger.info(\"hello\")"));
    assert!(!text.contains("console.log"));
}

#[test]
fn apply_truncated_preview_is_refused() {
    if !sg_available() {
        return;
    }
    let dir = default_fixture();
    let before = contents(&target(&dir));
    let result = run(&base_input(
        dir.path(),
        &[("apply", json!(true)), ("maxMatches", json!(2))],
    ));
    assert_eq!(result["ok"], json!(false));
    assert_eq!(result["error"]["code"], json!("PREVIEW_TRUNCATED"));
    assert_eq!(result["error"]["phase"], json!("preview"));
    assert_eq!(contents(&target(&dir)), before);
}

#[test]
fn apply_zero_matches_skips_second_pass() {
    if !sg_available() {
        return;
    }
    let dir = default_fixture();
    let before = contents(&target(&dir));
    let payload = run(&base_input(
        dir.path(),
        &[
            ("apply", json!(true)),
            ("pattern", json!("process.exit($CODE)")),
            ("rewrite", json!("shutdown($CODE)")),
        ],
    ));
    assert_eq!(payload["ok"], json!(true));
    assert_eq!(payload["applied"], json!(false));
    assert_eq!(payload["counts"]["plannedMatches"], json!(0));
    assert_eq!(payload["application"]["requested"], json!(true));
    assert_eq!(payload["application"]["performed"], json!(false));
    assert_eq!(payload["application"]["secondPassExitCode"], Value::Null);
    assert_eq!(contents(&target(&dir)), before);
}

#[test]
fn apply_after_fixture_changes_reports_stale_preview() {
    if !sg_available() {
        return;
    }
    let dir = fixture(&[("src/a.ts", "console.log(\"hello\");\n")]);
    let path = target(&dir);
    let rewrite_file = || std::fs::write(&path, "const untouched = 1;\n").unwrap();
    let result = execute_rewrite(
        &base_input(dir.path(), &[("apply", json!(true))]),
        SG_PATH,
        None,
        &RewriteHooks {
            on_preview_complete: Some(&rewrite_file),
        },
    );
    assert_eq!(result["ok"], json!(false));
    assert_eq!(result["error"]["code"], json!("REWRITE_STALE_PREVIEW"));
    assert_eq!(result["error"]["phase"], json!("apply"));
    assert_eq!(result["error"]["retryable"], json!(true));
    assert_eq!(read_text(&path), "const untouched = 1;\n");
}

#[test]
fn apply_after_deadline_expires_between_passes_times_out() {
    if !sg_available() {
        return;
    }
    let dir = fixture(&[("src/a.ts", "console.log(\"hello\");\n")]);
    let before = contents(&target(&dir));
    let stall = || std::thread::sleep(std::time::Duration::from_millis(1_100));
    let result = execute_rewrite(
        &base_input(
            dir.path(),
            &[("apply", json!(true)), ("timeoutMs", json!(1_000))],
        ),
        SG_PATH,
        None,
        &RewriteHooks {
            on_preview_complete: Some(&stall),
        },
    );
    assert_eq!(result["ok"], json!(false));
    assert_eq!(result["error"]["code"], json!("TIMEOUT"));
    assert!(result["durationMs"].as_u64().unwrap() >= 1_000);
    assert_eq!(contents(&target(&dir)), before);
    assert!(read_text(&target(&dir)).contains("console.log"));
}

#[test]
fn apply_malformed_pattern_reports_parse_failure() {
    if !sg_available() {
        return;
    }
    let dir = default_fixture();
    let before = contents(&target(&dir));
    let result = run(&base_input(
        dir.path(),
        &[
            ("apply", json!(true)),
            ("pattern", json!("console.log($MSG")),
            ("rewrite", json!("logger.info($MSG)")),
            ("force", json!(true)),
        ],
    ));
    assert_eq!(result["ok"], json!(false));
    assert_eq!(result["error"]["code"], json!("PATTERN_PARSE_FAILED"));
    assert_eq!(contents(&target(&dir)), before);
}
