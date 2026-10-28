//! Acceptance tests 9, 10, 12, 13, 14, 15, 20, 21, 22, 25 and 26 of
//! `docs/cleanroom/specs/ast-grep.md` §8 against the real `ast-grep` program. They fail, not skip,
//! when the program is missing.

mod common;

use common::{call, code_of, real_server, tree};
use serde_json::{Value, json};

fn read(dir: &std::path::Path, name: &str) -> String {
    std::fs::read_to_string(dir.join(name)).expect("read file")
}

fn warnings(value: &Value) -> Vec<String> {
    value["warnings"]
        .as_array()
        .map(|items| items.iter().filter_map(|w| w.as_str().map(str::to_owned)).collect())
        .unwrap_or_default()
}

#[tokio::test]
async fn t9_search_returns_relative_paths_lines_captures_and_sorted_matches() {
    let dir = tempfile::tempdir().expect("tempdir");
    tree(
        dir.path(),
        &[
            ("src/b.js", "x();\nalert(\"two\");\nalert(\"three\");\n"),
            ("a.js", "alert(\"one\");\n"),
        ],
    );
    let server = real_server(dir.path());
    let (value, is_error) = call(
        &server,
        "search",
        json!({ "paths": ["."], "pattern": "alert($TEXT)", "language": "javascript" }),
    )
    .await;
    assert!(!is_error, "{value}");
    assert_eq!(value["ok"], true);
    assert_eq!(value["kind"], "search");
    let matches = value["matches"].as_array().expect("matches");
    let paths: Vec<&str> = matches.iter().filter_map(|m| m["path"].as_str()).collect();
    assert_eq!(paths, ["a.js", "src/b.js", "src/b.js"]);
    assert_eq!(matches[0]["range"]["start"]["line"], 1);
    assert_eq!(matches[1]["range"]["start"]["line"], 2);
    assert_eq!(matches[2]["range"]["start"]["line"], 3);
    assert_eq!(matches[0]["metavariables"]["single"]["TEXT"], "\"one\"");
    assert_eq!(matches[0]["language"], "javascript");
    assert_eq!(value["counts"]["returnedMatches"], 3);
    assert_eq!(value["counts"]["returnedFiles"], 2);
    assert_eq!(value["counts"]["totalMatches"], 3);
    assert_eq!(value["truncation"]["truncated"], false);
}

#[tokio::test]
async fn t10_multi_capture_is_the_exact_source_slice() {
    let dir = tempfile::tempdir().expect("tempdir");
    tree(dir.path(), &[("c.js", "foo(1,2, 3);\n")]);
    let server = real_server(dir.path());
    let (value, _) = call(
        &server,
        "search",
        json!({ "paths": ["c.js"], "pattern": "foo($$$ITEMS)", "language": "javascript" }),
    )
    .await;
    assert_eq!(value["ok"], true, "{value}");
    assert_eq!(value["matches"][0]["metavariables"]["multi"]["ITEMS"], "1,2, 3");
}

#[tokio::test]
async fn t12_regex_syntax_and_lowercase_metavariables_are_rejected_unless_forced() {
    let dir = tempfile::tempdir().expect("tempdir");
    tree(dir.path(), &[("a.js", "var size = 1;\nlen(x);\n")]);
    let server = real_server(dir.path());
    let base = |pattern: &str, force: bool| {
        json!({ "paths": ["."], "pattern": pattern, "language": "javascript", "force": force })
    };
    let (rejected, is_error) = call(&server, "search", base("\\bsize", false)).await;
    assert!(is_error);
    assert_eq!(code_of(&rejected), "PATTERN_HINT_REJECTED");
    assert!(rejected["error"]["details"]["findings"].is_array());

    // With force the preflight lets it through, so the program (not the hint check) answers.
    let (forced, _) = call(&server, "search", base("\\bsize", true)).await;
    assert_ne!(code_of(&forced), "PATTERN_HINT_REJECTED", "{forced}");

    let (lower, _) = call(&server, "search", base("len($value)", false)).await;
    assert_eq!(code_of(&lower), "PATTERN_HINT_REJECTED");

    let (wildcard, is_error) = call(&server, "search", base("len($_)", false)).await;
    assert!(!is_error, "{wildcard}");
    assert_eq!(wildcard["counts"]["returnedMatches"], 1);
}

#[tokio::test]
async fn t13_python_class_head_with_colon_is_rejected() {
    let dir = tempfile::tempdir().expect("tempdir");
    tree(dir.path(), &[("a.py", "class A(B):\n    pass\n")]);
    let server = real_server(dir.path());
    let search = |pattern: &str| {
        json!({ "paths": ["."], "pattern": pattern, "language": "python" })
    };
    let (rejected, _) = call(&server, "search", search("class $K($$$):")).await;
    assert_eq!(code_of(&rejected), "PATTERN_HINT_REJECTED");
    let (accepted, _) = call(&server, "search", search("class $K($$$)")).await;
    assert_ne!(code_of(&accepted), "PATTERN_HINT_REJECTED", "{accepted}");
}

#[tokio::test]
async fn t14_error_node_pattern_fails_instead_of_returning_zero_matches() {
    let dir = tempfile::tempdir().expect("tempdir");
    tree(dir.path(), &[("a.js", "alert(1);\n")]);
    let server = real_server(dir.path());
    let (value, is_error) = call(
        &server,
        "search",
        json!({ "paths": ["."], "pattern": "alert($T", "language": "javascript" }),
    )
    .await;
    assert!(is_error, "{value}");
    assert_eq!(value["ok"], false);
    assert_eq!(code_of(&value), "PATTERN_PARSE_FAILED");
}

#[tokio::test]
async fn t15_match_limit_truncates_with_null_totals() {
    let dir = tempfile::tempdir().expect("tempdir");
    tree(dir.path(), &[("a.js", "hit(1);\nhit(2);\nhit(3);\nhit(4);\nhit(5);\n")]);
    let server = real_server(dir.path());
    let (value, is_error) = call(
        &server,
        "search",
        json!({ "paths": ["."], "pattern": "hit($A)", "language": "javascript", "maxMatches": 2 }),
    )
    .await;
    assert!(!is_error, "{value}");
    assert_eq!(value["matches"].as_array().map(Vec::len), Some(2));
    assert_eq!(value["truncation"]["truncated"], true);
    assert_eq!(value["truncation"]["reason"], "match_limit");
    assert_eq!(value["counts"]["totalMatches"], Value::Null);
    assert_eq!(value["counts"]["totalFiles"], Value::Null);
    assert_eq!(value["counts"]["atLeastMatches"], 3);
    assert!(warnings(&value).iter().any(|w| w.contains("narrow")));
}

#[tokio::test]
async fn t20_rewrite_dry_run_leaves_files_unchanged() {
    let dir = tempfile::tempdir().expect("tempdir");
    tree(dir.path(), &[("a.js", "old(1);\nold(2);\n")]);
    let server = real_server(dir.path());
    let (value, is_error) = call(
        &server,
        "rewrite",
        json!({ "paths": ["."], "pattern": "old($X)", "rewrite": "fresh($X)", "language": "javascript" }),
    )
    .await;
    assert!(!is_error, "{value}");
    assert_eq!(value["applied"], false);
    assert_eq!(value["counts"]["plannedMatches"], 2);
    assert_eq!(value["counts"]["plannedFiles"], 1);
    assert_eq!(value["matches"][0]["replacement"], "fresh(1)");
    assert_eq!(value["application"]["requested"], false);
    assert_eq!(value["application"]["idempotencyChecked"], false);
    assert_eq!(read(dir.path(), "a.js"), "old(1);\nold(2);\n");
}

#[tokio::test]
async fn t21_rewrite_apply_changes_files_with_preview_counts() {
    let dir = tempfile::tempdir().expect("tempdir");
    tree(dir.path(), &[("a.js", "old(1);\nold(2);\n")]);
    let server = real_server(dir.path());
    let (value, is_error) = call(
        &server,
        "rewrite",
        json!({ "paths": ["."], "pattern": "old($X)", "rewrite": "fresh($X)", "language": "javascript", "apply": true }),
    )
    .await;
    assert!(!is_error, "{value}");
    assert_eq!(value["applied"], true);
    assert_eq!(value["application"]["performed"], true);
    assert_eq!(value["application"]["countsArePreviewBased"], true);
    assert!(value["application"]["secondPassExitCode"].is_number());
    assert_eq!(value["counts"]["plannedMatches"], 2);
    assert_eq!(read(dir.path(), "a.js"), "fresh(1);\nfresh(2);\n");
}

#[tokio::test]
async fn t22_rewrite_metavariable_errors_are_rejected_even_with_force() {
    let dir = tempfile::tempdir().expect("tempdir");
    tree(dir.path(), &[("a.js", "old(1);\n")]);
    // The check runs before the program, so no real program is involved; the real server keeps
    // the call path identical to the other rewrite tests.
    let server = real_server(dir.path());
    let (unbound, is_error) = call(
        &server,
        "rewrite",
        json!({ "paths": ["."], "pattern": "old($X)", "rewrite": "new($Y)", "language": "javascript", "force": true }),
    )
    .await;
    assert!(is_error);
    assert_eq!(code_of(&unbound), "REWRITE_UNBOUND_METAVARIABLE");
    let (mismatch, _) = call(
        &server,
        "rewrite",
        json!({ "paths": ["."], "pattern": "old($$$X)", "rewrite": "new($X)", "language": "javascript", "force": true }),
    )
    .await;
    assert_eq!(code_of(&mismatch), "REWRITE_METAVARIABLE_KIND_MISMATCH");
    let (reverse, _) = call(
        &server,
        "rewrite",
        json!({ "paths": ["."], "pattern": "old($X)", "rewrite": "new($$$X)", "language": "javascript" }),
    )
    .await;
    assert_eq!(code_of(&reverse), "REWRITE_METAVARIABLE_KIND_MISMATCH");
    assert_eq!(read(dir.path(), "a.js"), "old(1);\n");
}

#[tokio::test]
async fn t25_scan_rule_source_must_be_exactly_one_and_yaml_must_parse() {
    let dir = tempfile::tempdir().expect("tempdir");
    tree(dir.path(), &[("a.js", "alert(1);\n"), ("rule.yml", "id: x\nlanguage: javascript\nrule:\n  pattern: alert($A)\n")]);
    let server = real_server(dir.path());
    let (both, is_error) = call(
        &server,
        "scan",
        json!({ "paths": ["."], "ruleFile": "rule.yml", "inlineRules": "id: x" }),
    )
    .await;
    assert!(is_error);
    assert_eq!(code_of(&both), "INVALID_ARGUMENT");
    let (neither, _) = call(&server, "scan", json!({ "paths": ["."] })).await;
    assert_eq!(code_of(&neither), "INVALID_ARGUMENT");
    let (broken, is_error) = call(
        &server,
        "scan",
        json!({ "paths": ["."], "inlineRules": "id: [" }),
    )
    .await;
    assert!(is_error, "{broken}");
    assert_eq!(code_of(&broken), "RULE_PARSE_FAILED");
    let (from_file, is_error) = call(
        &server,
        "scan",
        json!({ "paths": ["a.js"], "ruleFile": "rule.yml" }),
    )
    .await;
    assert!(!is_error, "{from_file}");
    assert_eq!(from_file["counts"]["plannedMatches"], 1);
}

#[tokio::test]
async fn t26_scan_with_fix_reports_rule_and_replacement_and_applies() {
    let dir = tempfile::tempdir().expect("tempdir");
    tree(dir.path(), &[("a.js", "alert(1);\n"), ("sgconfig.yml", "ruleDirs: [\n")]);
    let server = real_server(dir.path());
    let rules = "id: no-alert\nlanguage: javascript\nrule:\n  pattern: alert($A)\nfix: console.log($A)\n";
    let (preview, is_error) = call(
        &server,
        "scan",
        json!({ "paths": ["a.js"], "inlineRules": rules }),
    )
    .await;
    assert!(!is_error, "a broken sgconfig.yml must not matter: {preview}");
    assert_eq!(preview["kind"], "scan");
    assert_eq!(preview["matches"][0]["rule"]["ruleId"], "no-alert");
    assert!(preview["matches"][0]["rule"]["labels"].is_array());
    assert_eq!(preview["matches"][0]["replacement"], "console.log(1)");
    assert_eq!(read(dir.path(), "a.js"), "alert(1);\n");

    let (applied, is_error) = call(
        &server,
        "scan",
        json!({ "paths": ["a.js"], "inlineRules": rules, "apply": true }),
    )
    .await;
    assert!(!is_error, "{applied}");
    assert_eq!(applied["applied"], true);
    assert_eq!(read(dir.path(), "a.js"), "console.log(1);\n");
}

#[tokio::test]
async fn t23_apply_refuses_a_truncated_preview_and_accepts_zero_matches() {
    let dir = tempfile::tempdir().expect("tempdir");
    let text = "old(1);\nold(2);\nold(3);\nold(4);\nold(5);\n";
    tree(dir.path(), &[("a.js", text)]);
    let server = real_server(dir.path());
    let (truncated, is_error) = call(
        &server,
        "rewrite",
        json!({ "paths": ["."], "pattern": "old($X)", "rewrite": "fresh($X)", "language": "javascript",
                "apply": true, "maxMatches": 2 }),
    )
    .await;
    assert!(is_error, "{truncated}");
    assert_eq!(code_of(&truncated), "PREVIEW_TRUNCATED");
    assert_eq!(truncated["error"]["phase"], "apply");
    assert_eq!(read(dir.path(), "a.js"), text);

    let (none, is_error) = call(
        &server,
        "rewrite",
        json!({ "paths": ["."], "pattern": "absent($X)", "rewrite": "fresh($X)", "language": "javascript", "apply": true }),
    )
    .await;
    assert!(!is_error, "{none}");
    assert_eq!(none["applied"], false);
    assert_eq!(none["application"]["requested"], true);
    assert_eq!(none["application"]["performed"], false);
    assert!(!warnings(&none).is_empty());
    assert_eq!(read(dir.path(), "a.js"), text);
}
