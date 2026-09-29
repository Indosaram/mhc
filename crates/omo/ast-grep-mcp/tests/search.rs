mod common;

use ast_grep_mcp::tools::search::SEARCH_TOOL_DESCRIPTION;
use ast_grep_mcp::tools::search::SEARCH_TOOL_NAME;
use ast_grep_mcp::tools::search::SearchInput;
use ast_grep_mcp::tools::search::build_search_args;
use ast_grep_mcp::tools::search::execute_search;
use ast_grep_mcp::tools::search::parse_search_input;
use common::SG_PATH;
use common::fixture_repo;
use common::path_str;
use common::sg_available;
use pretty_assertions::assert_eq;
use serde_json::Map;
use serde_json::Value;
use serde_json::json;

fn base() -> Map<String, Value> {
    json!({ "pattern": "console.log($MSG)", "language": "typescript", "paths": ["src"] })
        .as_object()
        .cloned()
        .expect("object")
}

fn with(key: &str, value: Value) -> Value {
    let mut input = base();
    input.insert(key.to_owned(), value);
    Value::Object(input)
}

fn parse(value: Value) -> Result<SearchInput, String> {
    parse_search_input(&value)
}

fn astral(count: usize) -> String {
    "\u{1d400}".repeat(count)
}

#[test]
fn tool_name_is_search() {
    assert_eq!(SEARCH_TOOL_NAME, "search");
}

#[test]
fn description_is_verbatim() {
    let verbatim = "Search code structurally with ast-grep. The pattern is code, not regex, and must parse as one AST node in the required language; use narrow paths. `$NAME` and `$_` match one whole node, while `$$$NAME` and `$$$` match zero-or-more nodes. Names are uppercase, `$$NAME` is invalid, partial-token captures do not work, and a repeated metavariable must match identical code. Wrap non-standalone syntax and use `selector` when needed. Parse warnings mean the query failed, not that the code is absent.";
    assert_eq!(SEARCH_TOOL_DESCRIPTION, verbatim);
}

#[test]
fn schema_accepts_valid_input() {
    assert!(parse(Value::Object(base())).is_ok());
}

#[test]
fn schema_rejects_empty_pattern() {
    assert!(parse(with("pattern", json!(""))).is_err());
}

#[test]
fn schema_rejects_pattern_over_16kib() {
    assert!(parse(with("pattern", json!("x".repeat(16385)))).is_err());
}

#[test]
fn schema_rejects_missing_paths() {
    assert!(parse(json!({ "pattern": "console.log($MSG)", "language": "typescript" })).is_err());
}

#[test]
fn schema_rejects_empty_paths() {
    assert!(parse(with("paths", json!([]))).is_err());
}

#[test]
fn schema_rejects_65_paths() {
    let paths: Vec<String> = (0..65).map(|i| format!("p{i}")).collect();
    assert!(parse(with("paths", json!(paths))).is_err());
}

#[test]
fn schema_rejects_path_over_4096_chars() {
    assert!(parse(with("paths", json!(["x".repeat(4097)]))).is_err());
}

#[test]
fn schema_accepts_astral_path_at_4096_code_points() {
    assert!(parse(with("paths", json!([astral(4096)]))).is_ok());
}

#[test]
fn schema_rejects_astral_path_at_4097_code_points() {
    assert!(parse(with("paths", json!([astral(4097)]))).is_err());
}

#[test]
fn schema_rejects_invalid_language() {
    assert!(parse(with("language", json!("foobar"))).is_err());
}

#[test]
fn schema_rejects_strictness_template() {
    assert!(parse(with("strictness", json!("template"))).is_err());
}

#[test]
fn schema_accepts_strictness_cst() {
    assert_eq!(
        parse(with("strictness", json!("cst"))).unwrap().strictness,
        "cst"
    );
}

#[test]
fn schema_accepts_strictness_signature() {
    assert_eq!(
        parse(with("strictness", json!("signature")))
            .unwrap()
            .strictness,
        "signature"
    );
}

#[test]
fn schema_defaults_strictness_to_smart() {
    assert_eq!(parse(Value::Object(base())).unwrap().strictness, "smart");
}

#[test]
fn schema_rejects_max_matches_0() {
    assert!(parse(with("maxMatches", json!(0))).is_err());
}

#[test]
fn schema_rejects_max_matches_501() {
    assert!(parse(with("maxMatches", json!(501))).is_err());
}

#[test]
fn schema_accepts_max_matches_1() {
    assert_eq!(parse(with("maxMatches", json!(1))).unwrap().max_matches, 1);
}

#[test]
fn schema_accepts_max_matches_500() {
    assert_eq!(
        parse(with("maxMatches", json!(500))).unwrap().max_matches,
        500
    );
}

#[test]
fn schema_rejects_timeout_0() {
    assert!(parse(with("timeoutMs", json!(0))).is_err());
}

#[test]
fn schema_rejects_timeout_999() {
    assert!(parse(with("timeoutMs", json!(999))).is_err());
}

#[test]
fn schema_rejects_fractional_timeout() {
    assert!(parse(with("timeoutMs", json!(1000.5))).is_err());
}

#[test]
fn schema_rejects_timeout_300001() {
    assert!(parse(with("timeoutMs", json!(300001))).is_err());
}

#[test]
fn schema_accepts_timeout_1000() {
    assert_eq!(
        parse(with("timeoutMs", json!(1000))).unwrap().timeout_ms,
        1000
    );
}

#[test]
fn schema_accepts_timeout_300000() {
    assert_eq!(
        parse(with("timeoutMs", json!(300000))).unwrap().timeout_ms,
        300000
    );
}

#[test]
fn schema_defaults_timeout_to_300000() {
    assert_eq!(parse(Value::Object(base())).unwrap().timeout_ms, 300000);
}

#[test]
fn schema_defaults_max_matches_to_50() {
    assert_eq!(parse(Value::Object(base())).unwrap().max_matches, 50);
}

#[test]
fn schema_rejects_additional_properties() {
    assert!(parse(with("bogus", json!(true))).is_err());
}

#[test]
fn schema_rejects_33_globs() {
    assert!(parse(with("globs", json!(vec!["*.ts"; 33]))).is_err());
}

#[test]
fn schema_rejects_glob_over_1024_chars() {
    assert!(parse(with("globs", json!(["x".repeat(1025)]))).is_err());
}

#[test]
fn schema_accepts_astral_glob_at_1024_code_points() {
    assert!(parse(with("globs", json!([astral(1024)]))).is_ok());
}

#[test]
fn schema_rejects_astral_glob_at_1025_code_points() {
    assert!(parse(with("globs", json!([astral(1025)]))).is_err());
}

#[test]
fn schema_rejects_empty_selector() {
    assert!(parse(with("selector", json!(""))).is_err());
}

#[test]
fn schema_rejects_selector_over_128_chars() {
    assert!(parse(with("selector", json!("x".repeat(129)))).is_err());
}

#[test]
fn schema_accepts_astral_selector_at_128_code_points() {
    assert!(parse(with("selector", json!(astral(128)))).is_ok());
}

#[test]
fn schema_rejects_astral_selector_at_129_code_points() {
    assert!(parse(with("selector", json!(astral(129)))).is_err());
}

#[test]
fn schema_rejects_empty_workdir() {
    assert!(parse(with("workdir", json!(""))).is_err());
}

#[test]
fn schema_rejects_workdir_over_4096_chars() {
    assert!(parse(with("workdir", json!("x".repeat(4097)))).is_err());
}

#[test]
fn schema_accepts_astral_workdir_at_4096_code_points() {
    assert!(parse(with("workdir", json!(astral(4096)))).is_ok());
}

#[test]
fn schema_rejects_astral_workdir_at_4097_code_points() {
    assert!(parse(with("workdir", json!(astral(4097)))).is_err());
}

#[test]
fn schema_pattern_limit_message_counts_bytes() {
    let error = parse(with("pattern", json!("x".repeat(16385)))).unwrap_err();
    assert_eq!(error, "pattern must be at most 16384 bytes");
}

fn minimal_input(paths: &[&str]) -> SearchInput {
    parse(with("paths", json!(paths))).expect("valid input")
}

#[test]
fn cli_args_minimal() {
    assert_eq!(
        build_search_args(&minimal_input(&["src"])),
        vec![
            "run",
            "-p",
            "console.log($MSG)",
            "--lang",
            "typescript",
            "--json=stream",
            "--strictness",
            "smart",
            "src"
        ]
    );
}

#[test]
fn cli_args_all_optional_flags() {
    let input = parse(json!({
        "pattern": "console.log($MSG)",
        "language": "typescript",
        "paths": ["src", "lib"],
        "strictness": "cst",
        "globs": ["*.ts", "!*.test.ts"],
        "selector": "expression",
        "includeHidden": true,
        "followSymlinks": true,
    }))
    .unwrap();
    assert_eq!(
        build_search_args(&input),
        vec![
            "run",
            "-p",
            "console.log($MSG)",
            "--lang",
            "typescript",
            "--json=stream",
            "--strictness",
            "cst",
            "--selector",
            "expression",
            "--globs",
            "*.ts",
            "--globs",
            "!*.test.ts",
            "--no-ignore",
            "hidden",
            "--follow",
            "src",
            "lib"
        ]
    );
}

#[test]
fn cli_args_without_include_hidden_omit_no_ignore() {
    assert!(!build_search_args(&minimal_input(&["src"])).contains(&"--no-ignore".to_owned()));
}

#[test]
fn cli_args_without_globs_omit_globs() {
    assert!(!build_search_args(&minimal_input(&["src"])).contains(&"--globs".to_owned()));
}

#[test]
fn cli_args_never_include_context_flag() {
    assert!(!build_search_args(&minimal_input(&["src"])).contains(&"-C".to_owned()));
}

fn live(pattern: &str, dir: &std::path::Path, extra: &[(&str, Value)]) -> Value {
    let mut input = json!({
        "pattern": pattern,
        "language": "typescript",
        "paths": [path_str(&dir.join("src"))],
        "workdir": path_str(dir),
    })
    .as_object()
    .cloned()
    .unwrap();
    for (key, value) in extra {
        input.insert((*key).to_owned(), value.clone());
    }
    let parsed = parse(Value::Object(input)).expect("valid input");
    execute_search(&parsed, SG_PATH, None)
}

#[test]
fn live_success_payload_contract() {
    if !sg_available() {
        return;
    }
    let dir = fixture_repo();
    let payload = live("console.log($MSG)", dir.path(), &[]);
    assert_eq!(payload["ok"], json!(true));
    assert_eq!(payload["schemaVersion"], json!(1));
    assert_eq!(payload["kind"], json!("search"));
    assert_eq!(payload["workdir"], json!(path_str(dir.path())));
    let matches = payload["matches"].as_array().unwrap();
    assert_eq!(matches.len(), 4);
    assert!(
        matches
            .iter()
            .all(|m| m["path"].as_str().unwrap().starts_with("src/"))
    );
    assert!(
        matches
            .iter()
            .all(|m| m["metavariables"]["single"]["MSG"].is_string())
    );
    assert_eq!(
        payload["counts"],
        json!({ "returnedMatches": 4, "returnedFiles": 2, "totalMatches": 4, "totalFiles": 2, "atLeastMatches": 4 })
    );
    assert_eq!(
        payload["truncation"],
        json!({ "truncated": false, "reason": null, "maxMatches": 50, "maxPayloadBytes": 4 * 1024 * 1024, "salvagedRecords": 0 })
    );
    assert_eq!(payload["warnings"], json!([]));
    assert!(payload["durationMs"].is_u64());
    for flat in ["count", "atLeastMatches", "truncationReason"] {
        assert!(payload.get(flat).is_none(), "unexpected flat key {flat}");
    }
}

#[test]
fn live_max_matches_1_truncates_with_limit_warning() {
    if !sg_available() {
        return;
    }
    let dir = fixture_repo();
    let payload = live("console.log($MSG)", dir.path(), &[("maxMatches", json!(1))]);
    assert_eq!(payload["ok"], json!(true));
    assert_eq!(payload["matches"].as_array().unwrap().len(), 1);
    assert_eq!(payload["truncation"]["truncated"], json!(true));
    assert_eq!(payload["truncation"]["reason"], json!("match_limit"));
    assert_eq!(payload["truncation"]["maxMatches"], json!(1));
    assert_eq!(payload["counts"]["atLeastMatches"], json!(2));
    assert_eq!(payload["counts"]["totalMatches"], Value::Null);
    assert_eq!(payload["counts"]["totalFiles"], Value::Null);
    assert_eq!(payload["counts"]["returnedMatches"], json!(1));
    assert!(
        payload["warnings"]
            .as_array()
            .unwrap()
            .contains(&json!("Result limit reached; narrow paths or globs."))
    );
}

#[test]
fn live_malformed_pattern_returns_error_payload() {
    if !sg_available() {
        return;
    }
    let dir = fixture_repo();
    let payload = live("console.log($MSG", dir.path(), &[]);
    assert_eq!(payload["ok"], json!(false));
    assert_eq!(payload["schemaVersion"], json!(1));
    let error = &payload["error"];
    assert_eq!(error["code"], json!("PATTERN_PARSE_FAILED"));
    assert!(!error["message"].as_str().unwrap().is_empty());
    assert_eq!(error["retryable"], json!(false));
    assert_eq!(error["phase"], json!("search"));
    assert_eq!(error["language"], json!("typescript"));
    assert!(
        error["details"]["stderr"]
            .as_str()
            .unwrap()
            .contains("ERROR node")
    );
    assert!(error["details"]["hint"].is_string());
    assert!(payload.get("code").is_none());
    assert!(payload.get("message").is_none());
}

#[test]
fn live_no_matches_returns_empty_success() {
    if !sg_available() {
        return;
    }
    let dir = fixture_repo();
    let payload = live("process.exit($CODE)", dir.path(), &[]);
    assert_eq!(payload["ok"], json!(true));
    assert_eq!(payload["matches"], json!([]));
    assert_eq!(
        payload["counts"],
        json!({ "returnedMatches": 0, "returnedFiles": 0, "totalMatches": 0, "totalFiles": 0, "atLeastMatches": 0 })
    );
    assert_eq!(payload["truncation"]["truncated"], json!(false));
}

#[test]
fn live_include_hidden_returns_hidden_files() {
    if !sg_available() {
        return;
    }
    let dir = fixture_repo();
    std::fs::write(
        dir.path().join("src/.hidden.ts"),
        "console.log(\"hidden\");\n",
    )
    .unwrap();
    let payload = live(
        "console.log($MSG)",
        dir.path(),
        &[("includeHidden", json!(true))],
    );
    assert_eq!(payload["ok"], json!(true));
    let matches = payload["matches"].as_array().unwrap();
    assert!(matches.len() >= 5);
    assert!(
        matches
            .iter()
            .any(|m| m["path"].as_str().unwrap().contains(".hidden"))
    );
}
