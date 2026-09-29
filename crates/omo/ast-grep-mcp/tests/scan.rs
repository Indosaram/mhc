mod common;

use std::path::Path;
use std::process::Command;

use ast_grep_mcp::tools::scan::RuleSource;
use ast_grep_mcp::tools::scan::ScanInput;
use ast_grep_mcp::tools::scan::build_scan_apply_args;
use ast_grep_mcp::tools::scan::build_scan_args;
use ast_grep_mcp::tools::scan::execute_scan;
use ast_grep_mcp::tools::scan::parse_scan_input;
use common::SG_PATH;
use common::path_str;
use common::sg_available;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;

const RULE: &str = "id: no-console
language: TypeScript
severity: warning
message: Avoid console.log
note: Use logger.info instead
labels:
  MSG:
    style: primary
    message: logged value
metadata:
  category: quality
rule:
  pattern: console.log($MSG)
fix: logger.info($MSG)
";
const ASTRAL: &str = "\u{1D11E}";
const DEFAULT_SOURCE: &str = "console.log(\"hello\");\nconst x = 1;\nconsole.log(x);\n";

fn fixture(contents: &str) -> tempfile::TempDir {
    let dir = tempfile::Builder::new()
        .prefix("omo-scan-")
        .tempdir()
        .unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/a.ts"), contents).unwrap();
    std::fs::write(dir.path().join("rule.yml"), RULE).unwrap();
    dir
}

fn base_value(dir: &Path, overrides: &[(&str, Value)]) -> Value {
    let mut input = json!({
        "ruleFile": path_str(&dir.join("rule.yml")),
        "paths": [path_str(&dir.join("src"))],
        "workdir": path_str(dir),
    });
    for (key, value) in overrides {
        input
            .as_object_mut()
            .unwrap()
            .insert((*key).to_owned(), value.clone());
    }
    input
}

fn base_input(dir: &Path) -> ScanInput {
    parse_scan_input(&base_value(dir, &[])).unwrap()
}

fn inline_value(dir: &Path, rules: &str) -> Value {
    json!({
        "inlineRules": rules,
        "paths": [path_str(&dir.join("src"))],
        "workdir": path_str(dir),
    })
}

fn contents(path: &Path) -> Vec<u8> {
    std::fs::read(path).unwrap()
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

#[test]
fn schema_rejects_both_rule_sources() {
    assert!(
        parse_scan_input(&json!({ "ruleFile": "rule.yml", "inlineRules": RULE, "paths": ["src"] }))
            .is_err()
    );
}

#[test]
fn schema_rejects_neither_rule_source() {
    assert!(parse_scan_input(&json!({ "paths": ["src"] })).is_err());
}

#[test]
fn one_of_violation_returns_invalid_argument_before_spawn() {
    let both = execute_scan(
        &json!({ "ruleFile": "rule.yml", "inlineRules": RULE, "paths": ["src"] }),
        "/missing/sg",
        None,
    );
    let neither = execute_scan(&json!({ "paths": ["src"] }), "/missing/sg", None);
    assert_eq!(both["ok"], json!(false));
    assert_eq!(both["error"]["code"], json!("INVALID_ARGUMENT"));
    assert_eq!(neither["ok"], json!(false));
    assert_eq!(neither["error"]["code"], json!("INVALID_ARGUMENT"));
}

#[test]
fn schema_defaults() {
    let input = parse_scan_input(&json!({ "inlineRules": RULE, "paths": ["src"] })).unwrap();
    assert!(!input.apply);
    assert!(!input.include_metadata);
    assert_eq!(input.max_matches, 50);
    assert_eq!(input.timeout_ms, 300_000);
}

#[test]
fn schema_rejects_values_outside_shared_bounds() {
    let cases = [
        json!({ "inlineRules": RULE, "paths": ["x".repeat(4_097)] }),
        json!({ "inlineRules": RULE, "paths": ["src"], "workdir": "x".repeat(4_097) }),
        json!({ "inlineRules": RULE, "paths": ["src"], "globs": ["x".repeat(1_025)] }),
        json!({ "inlineRules": RULE, "paths": ["src"], "timeoutMs": 999 }),
        json!({ "inlineRules": RULE, "paths": ["src"], "maxMatches": 501 }),
    ];
    for case in cases {
        assert!(parse_scan_input(&case).is_err(), "{case}");
    }
}

#[test]
fn schema_rejects_oversized_rule_sources() {
    assert!(parse_scan_input(&json!({ "ruleFile": "x".repeat(4_097), "paths": ["src"] })).is_err());
    assert!(
        parse_scan_input(&json!({ "inlineRules": "x".repeat(64 * 1_024 + 1), "paths": ["src"] }))
            .is_err()
    );
}

#[test]
fn rule_file_args_keep_json_preview_only() {
    let project = std::env::temp_dir().join("project");
    let rule_file = path_str(&project.join("rule.yml"));
    let source_path = path_str(&project.join("src"));
    let input = base_input(&project);
    assert_eq!(
        build_scan_args(&input),
        strings(&["scan", "--rule", &rule_file, "--json=stream", &source_path])
    );
    assert_eq!(
        build_scan_apply_args(&input),
        strings(&["scan", "--rule", &rule_file, "--update-all", &source_path])
    );
}

#[test]
fn inline_rules_args_stay_explicit_and_use_metadata_flag() {
    let rules = format!(
        "{RULE}\n---\n{}",
        RULE.replace("no-console", "no-console-2")
    );
    let input = parse_scan_input(&json!({
        "inlineRules": rules,
        "paths": ["src"],
        "includeMetadata": true,
        "globs": ["*.ts"],
        "includeHidden": true,
        "followSymlinks": true,
    }))
    .unwrap();
    let args = build_scan_args(&input);
    assert_eq!(input.source, RuleSource::Inline(rules.clone()));
    assert_eq!(args[..3], strings(&["scan", "--inline-rules", &rules])[..]);
    for expected in ["--include-metadata", "--globs", "--no-ignore", "--follow"] {
        assert!(args.contains(&expected.to_owned()), "{expected}");
    }
    assert!(!args.contains(&"--report-style".to_owned()));
}

type AstralField = (&'static str, usize, fn(String) -> Value);

fn astral_fields() -> Vec<AstralField> {
    vec![
        (
            "ruleFile",
            4_096,
            |value| json!({ "ruleFile": value, "paths": ["src"] }),
        ),
        (
            "path",
            4_096,
            |value| json!({ "inlineRules": RULE, "paths": [value] }),
        ),
        (
            "workdir",
            4_096,
            |value| json!({ "inlineRules": RULE, "paths": ["src"], "workdir": value }),
        ),
        (
            "glob",
            1_024,
            |value| json!({ "inlineRules": RULE, "paths": ["src"], "globs": [value] }),
        ),
    ]
}

#[test]
fn astral_character_is_one_code_point_two_utf16_units() {
    assert_eq!(ASTRAL.chars().count(), 1);
    assert_eq!(ASTRAL.encode_utf16().count(), 2);
}

#[test]
fn astral_exact_limit_is_accepted() {
    for (field, limit, build) in astral_fields() {
        let value = ASTRAL.repeat(limit);
        assert_eq!(value.chars().count(), limit);
        assert_eq!(value.encode_utf16().count(), limit * 2);
        assert!(parse_scan_input(&build(value)).is_ok(), "{field}");
    }
}

#[test]
fn astral_limit_plus_one_is_rejected() {
    for (field, limit, build) in astral_fields() {
        let value = ASTRAL.repeat(limit + 1);
        assert_eq!(value.chars().count(), limit + 1);
        assert_eq!(value.encode_utf16().count(), (limit + 1) * 2);
        assert!(parse_scan_input(&build(value)).is_err(), "{field}");
    }
}

#[cfg(unix)]
#[test]
fn sg_deprecation_banner_is_tolerated_as_warning() {
    let dir = fixture(DEFAULT_SOURCE);
    let record = json!({
        "text": "console.log(x)",
        "range": { "byteOffset": { "start": 0, "end": 14 }, "start": { "line": 0, "column": 0 }, "end": { "line": 0, "column": 14 } },
        "file": "src/a.ts",
        "replacement": "logger.info(x)",
        "replacementOffsets": { "start": 0, "end": 14 },
        "language": "TypeScript",
        "metaVariables": { "single": {}, "multi": {}, "transformed": {} },
        "ruleId": "no-console",
        "severity": "warning",
        "labels": [],
    })
    .to_string()
    .replace('\'', "'\\''");
    let fake_sg = common::write_script(
        dir.path(),
        "fake-sg.sh",
        &format!(
            "printf '%s\\n' 'WARNING: the sg command name is deprecated; use ast-grep' >&2\nprintf '%s\\n' '{record}'"
        ),
    );
    let result = execute_scan(&base_value(dir.path(), &[]), &fake_sg, None);
    assert_eq!(result["ok"], json!(true), "{result}");
    assert!(
        result["warnings"][0]
            .as_str()
            .unwrap()
            .starts_with("WARNING:")
    );
}

#[test]
fn inline_rule_dry_scan_carries_nested_rule_block() {
    if !sg_available() {
        return;
    }
    let dir = fixture(DEFAULT_SOURCE);
    let target = dir.path().join("src/a.ts");
    let before = contents(&target);
    let payload = execute_scan(&inline_value(dir.path(), RULE), SG_PATH, None);
    assert_eq!(payload["ok"], json!(true), "{payload}");
    assert_eq!(payload["applied"], json!(false));
    let first = &payload["matches"][0];
    assert_eq!(payload["matches"].as_array().unwrap().len(), 2);
    assert_eq!(first["path"], json!("src/a.ts"));
    assert_eq!(
        first["range"]["start"],
        json!({ "line": 1, "column": 0, "byteOffset": 0 })
    );
    assert_eq!(first["replacement"], json!("logger.info(\"hello\")"));
    assert_eq!(first["rule"]["ruleId"], json!("no-console"));
    assert_eq!(first["rule"]["severity"], json!("warning"));
    assert_eq!(first["rule"]["note"], json!("Use logger.info instead"));
    assert_eq!(first["rule"]["message"], json!("Avoid console.log"));
    assert_eq!(first["rule"]["labels"].as_array().unwrap().len(), 1);
    assert!(first["rule"].get("metadata").is_none());
    assert_eq!(contents(&target), before);
}

#[test]
fn metadata_present_only_when_requested() {
    if !sg_available() {
        return;
    }
    let dir = fixture(DEFAULT_SOURCE);
    let without = execute_scan(&base_value(dir.path(), &[]), SG_PATH, None);
    let with_metadata = execute_scan(
        &base_value(dir.path(), &[("includeMetadata", json!(true))]),
        SG_PATH,
        None,
    );
    assert!(without["matches"][0]["rule"].get("metadata").is_none());
    assert_eq!(
        with_metadata["matches"][0]["rule"]["metadata"],
        json!({ "category": "quality" })
    );
}

fn run_sg(dir: &Path, args: &[String]) -> std::process::Output {
    Command::new(SG_PATH)
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap()
}

#[test]
fn apply_matrix_json_update_does_not_mutate_but_plain_update_does() {
    if !sg_available() {
        return;
    }
    let dir = fixture("console.log(\"hello\");\n");
    let target = dir.path().join("src/a.ts");
    let before = contents(&target);
    let rule = path_str(&dir.path().join("rule.yml"));
    let src = path_str(&dir.path().join("src"));
    let combo = run_sg(
        dir.path(),
        &strings(&[
            "scan",
            "--rule",
            &rule,
            "--update-all",
            "--json=stream",
            &src,
        ]),
    );
    assert_eq!(combo.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&combo.stdout).contains("\"replacement\":\"logger.info"));
    assert_eq!(contents(&target), before);
    let plain = run_sg(
        dir.path(),
        &strings(&["scan", "--rule", &rule, "--update-all", &src]),
    );
    assert_eq!(plain.status.code(), Some(0));
    assert_ne!(contents(&target), before);
    assert!(
        std::fs::read_to_string(&target)
            .unwrap()
            .contains("logger.info")
    );
}

#[test]
fn apply_runs_preview_then_plain_update_all() {
    if !sg_available() {
        return;
    }
    let dir = fixture("console.log(\"hello\");\n");
    let payload = execute_scan(
        &base_value(dir.path(), &[("apply", json!(true))]),
        SG_PATH,
        None,
    );
    assert_eq!(payload["ok"], json!(true), "{payload}");
    assert_eq!(payload["applied"], json!(true));
    assert_eq!(payload["application"]["performed"], json!(true));
    assert_eq!(payload["application"]["secondPassExitCode"], json!(0));
    assert!(
        std::fs::read_to_string(dir.path().join("src/a.ts"))
            .unwrap()
            .contains("logger.info")
    );
}

#[test]
fn no_matching_rule_is_valid_empty_success() {
    if !sg_available() {
        return;
    }
    let dir = fixture("const x = 1;\n");
    let direct = run_sg(dir.path(), &build_scan_args(&base_input(dir.path())));
    assert_eq!(direct.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&direct.stdout), "");
    let payload = execute_scan(&base_value(dir.path(), &[]), SG_PATH, None);
    assert_eq!(payload["ok"], json!(true));
    assert_eq!(payload["matches"], json!([]));
}

#[test]
fn unparseable_yaml_returns_rule_parse_failed() {
    if !sg_available() {
        return;
    }
    let dir = fixture(DEFAULT_SOURCE);
    let payload = execute_scan(&inline_value(dir.path(), "id: ["), SG_PATH, None);
    assert_eq!(payload["ok"], json!(false));
    assert_eq!(payload["error"]["code"], json!("RULE_PARSE_FAILED"));
}

#[test]
fn hostile_parent_sgconfig_is_isolated() {
    if !sg_available() {
        return;
    }
    let parent = fixture(DEFAULT_SOURCE);
    let child = parent.path().join("child");
    std::fs::create_dir_all(child.join("src")).unwrap();
    std::fs::write(child.join("src/a.ts"), "console.log(\"isolated\");\n").unwrap();
    std::fs::write(
        parent.path().join("sgconfig.yml"),
        "ruleDirs: [definitely-missing-rules]\n",
    )
    .unwrap();
    let payload = execute_scan(&inline_value(&child, RULE), SG_PATH, None);
    assert_eq!(payload["ok"], json!(true), "{payload}");
    assert_eq!(payload["matches"].as_array().unwrap().len(), 1);
}

#[test]
fn truncated_preview_refuses_mutation() {
    if !sg_available() {
        return;
    }
    let dir = fixture(DEFAULT_SOURCE);
    let target = dir.path().join("src/a.ts");
    let before = contents(&target);
    let payload = execute_scan(
        &base_value(
            dir.path(),
            &[("apply", json!(true)), ("maxMatches", json!(1))],
        ),
        SG_PATH,
        None,
    );
    assert_eq!(payload["ok"], json!(false));
    assert_eq!(payload["error"]["code"], json!("PREVIEW_TRUNCATED"));
    assert_eq!(contents(&target), before);
}
