use ast_grep_mcp::pattern_hints::HintSeverity;
use ast_grep_mcp::pattern_hints::ValidationOpts;
use ast_grep_mcp::pattern_hints::ValidationResult;
use ast_grep_mcp::pattern_hints::extract_metavars;
use ast_grep_mcp::pattern_hints::validate_pattern_hints;
use ast_grep_mcp::pattern_hints::validate_rewrite_hints;
use pretty_assertions::assert_eq;
use serde_json::json;

fn has_hint(result: &ValidationResult, code: &str) -> bool {
    result.hints.iter().any(|hint| hint.code == code)
}

fn plain() -> ValidationOpts<'static> {
    ValidationOpts::default()
}

fn forced() -> ValidationOpts<'static> {
    ValidationOpts {
        force: true,
        ..ValidationOpts::default()
    }
}

#[test]
fn extract_metavars_cases() {
    let cases: [(&str, &str, &[&str], &[&str]); 11] = [
        ("single metavar $MSG", "console.log($MSG)", &["MSG"], &[]),
        ("multi metavar $$$ARGS", "foo($$$ARGS)", &[], &["ARGS"]),
        ("mixed single and multi", "foo($A, $$$B)", &["A"], &["B"]),
        ("wildcard underscore excluded", "foo($_)", &[], &[]),
        ("multi wildcard excluded", "foo($$$)", &[], &[]),
        ("same name as single and multi", "$A + $$$A", &["A"], &["A"]),
        ("no metavars", "console.log(42)", &[], &[]),
        ("multiple singles", "$A + $B", &["A", "B"], &[]),
        ("multiple multis", "$$$X, $$$Y", &[], &["X", "Y"]),
        ("double dollar not captured", "$$ARGS", &[], &[]),
        ("lowercase not captured", "$foo", &[], &[]),
    ];
    for (name, text, single, multi) in cases {
        let result = extract_metavars(text);
        let mut got_single = result.single.clone();
        got_single.sort();
        let mut got_multi = result.multi.clone();
        got_multi.sort();
        assert_eq!(got_single, single.to_vec(), "{name}: single");
        assert_eq!(got_multi, multi.to_vec(), "{name}: multi");
    }
}

const HARD_REJECT: [(&str, &str, &str, &str); 16] = [
    (
        "regex \\w escape",
        "\\w+",
        "typescript",
        "REGEX_BACKSLASH_ESCAPE",
    ),
    (
        "regex \\d escape",
        "\\d+",
        "typescript",
        "REGEX_BACKSLASH_ESCAPE",
    ),
    (
        "regex \\s escape",
        "\\s+",
        "typescript",
        "REGEX_BACKSLASH_ESCAPE",
    ),
    (
        "regex \\b escape",
        "\\bword\\b",
        "typescript",
        "REGEX_BACKSLASH_ESCAPE",
    ),
    ("regex .* wildcard", ".*foo", "typescript", "REGEX_DOT_STAR"),
    ("regex .+ wildcard", ".+foo", "typescript", "REGEX_DOT_STAR"),
    (
        "regex char class [a-z]",
        "[a-z]",
        "typescript",
        "REGEX_CHAR_CLASS",
    ),
    (
        "regex char class [a-zA-Z0-9]",
        "[a-zA-Z0-9]",
        "typescript",
        "REGEX_CHAR_CLASS",
    ),
    (
        "python trailing colon def",
        "def foo($X):",
        "python",
        "PATTERN_INCOMPLETE_FORM",
    ),
    (
        "python trailing colon class",
        "class Foo($X):",
        "python",
        "PATTERN_INCOMPLETE_FORM",
    ),
    (
        "JS incomplete function",
        "function foo",
        "javascript",
        "PATTERN_INCOMPLETE_FORM",
    ),
    (
        "TS incomplete function",
        "function foo",
        "typescript",
        "PATTERN_INCOMPLETE_FORM",
    ),
    (
        "Go incomplete function",
        "func foo",
        "go",
        "PATTERN_INCOMPLETE_FORM",
    ),
    (
        "Rust incomplete function",
        "fn foo",
        "rust",
        "PATTERN_INCOMPLETE_FORM",
    ),
    (
        "double dollar $$ARGS",
        "$$ARGS",
        "typescript",
        "METAVAR_DOUBLE_DOLLAR",
    ),
    (
        "invalid metavar name lowercase",
        "$foo",
        "typescript",
        "INVALID_METAVAR_NAME",
    ),
];

#[test]
fn hard_reject_rules_reject_without_force() {
    for (name, pattern, language, code) in HARD_REJECT {
        let result = validate_pattern_hints(pattern, language, &plain());
        assert!(result.rejected && !result.ok, "{name}: rejected");
        assert_eq!(result.code, Some("PATTERN_HINT_REJECTED"), "{name}");
        assert!(has_hint(&result, code), "{name}: hint {code}");
    }
}

#[test]
fn hard_reject_rules_bypassed_by_force() {
    for (name, pattern, language, code) in HARD_REJECT {
        let result = validate_pattern_hints(pattern, language, &forced());
        assert!(!result.rejected && result.ok, "{name}: bypassed");
        assert_eq!(result.code, None, "{name}");
        assert!(has_hint(&result, code), "{name}: hint {code}");
    }
}

#[test]
fn bare_alternation_warns_but_does_not_reject() {
    let result = validate_pattern_hints("a|b", "typescript", &plain());
    assert!(!result.rejected && result.ok);
    assert!(
        result
            .hints
            .iter()
            .any(|hint| hint.code == "BARE_ALTERNATION" && hint.severity == HintSeverity::Warn)
    );
}

#[test]
fn bare_alternation_warns_even_with_force() {
    let result = validate_pattern_hints("a|b", "typescript", &forced());
    assert!(!result.rejected);
    assert!(has_hint(&result, "BARE_ALTERNATION"));
}

#[test]
fn double_pipe_is_not_flagged() {
    let result = validate_pattern_hints("a || b", "typescript", &plain());
    assert!(!has_hint(&result, "BARE_ALTERNATION"));
}

fn always_reject_cases() -> Vec<(
    &'static str,
    &'static str,
    &'static str,
    ValidationOpts<'static>,
    &'static str,
)> {
    static EMPTY_STRING: std::sync::LazyLock<serde_json::Value> =
        std::sync::LazyLock::new(|| json!(""));
    static EMPTY_ARRAY: std::sync::LazyLock<serde_json::Value> =
        std::sync::LazyLock::new(|| json!([]));
    let with_paths = |paths: &'static serde_json::Value| ValidationOpts {
        paths: Some(paths),
        ..ValidationOpts::default()
    };
    let with_limit = |limit: f64| ValidationOpts {
        limit: Some(limit),
        ..ValidationOpts::default()
    };
    vec![
        ("empty pattern", "", "typescript", plain(), "PATTERN_EMPTY"),
        (
            "whitespace-only pattern",
            "   ",
            "typescript",
            plain(),
            "PATTERN_EMPTY",
        ),
        (
            "unsupported language",
            "foo",
            "brainfuck",
            plain(),
            "LANGUAGE_UNSUPPORTED",
        ),
        (
            "invalid path - empty string",
            "foo",
            "typescript",
            with_paths(&EMPTY_STRING),
            "INVALID_PATH",
        ),
        (
            "invalid path - empty array",
            "foo",
            "typescript",
            with_paths(&EMPTY_ARRAY),
            "INVALID_PATH",
        ),
        (
            "invalid limit - negative",
            "foo",
            "typescript",
            with_limit(-1.0),
            "INVALID_LIMIT",
        ),
        (
            "invalid limit - zero",
            "foo",
            "typescript",
            with_limit(0.0),
            "INVALID_LIMIT",
        ),
        (
            "invalid limit - NaN",
            "foo",
            "typescript",
            with_limit(f64::NAN),
            "INVALID_LIMIT",
        ),
        (
            "invalid limit - Infinity",
            "foo",
            "typescript",
            with_limit(f64::INFINITY),
            "INVALID_LIMIT",
        ),
    ]
}

#[test]
fn always_reject_rules_reject_without_force() {
    for (name, pattern, language, opts, code) in always_reject_cases() {
        let result = validate_pattern_hints(pattern, language, &opts);
        assert!(result.rejected && !result.ok, "{name}: rejected");
        assert_eq!(result.code, Some(code), "{name}");
    }
}

#[test]
fn always_reject_rules_not_bypassed_by_force() {
    for (name, pattern, language, opts, code) in always_reject_cases() {
        let opts = ValidationOpts {
            force: true,
            ..opts
        };
        let result = validate_pattern_hints(pattern, language, &opts);
        assert!(result.rejected, "{name}: still rejected");
        assert_eq!(result.code, Some(code), "{name}");
    }
}

#[test]
fn happy_paths_are_accepted() {
    let cases = [
        (
            "console.log with metavar",
            "console.log($MSG)",
            "typescript",
        ),
        (
            "function with body",
            "function $NAME($$$ARGS) { $$$BODY }",
            "typescript",
        ),
        (
            "python def without trailing colon",
            "def $FUNC($$$)",
            "python",
        ),
        ("go func with body", "func $NAME($$$) { $$$ }", "go"),
        ("rust fn with body", "fn $NAME($$$) -> $RET { $$$ }", "rust"),
        ("alias ts -> typescript", "console.log($MSG)", "ts"),
        ("alias py -> python", "def $FUNC($$$)", "py"),
        (
            "alias js -> javascript",
            "function $NAME($$$) { $$$ }",
            "js",
        ),
    ];
    for (name, pattern, language) in cases {
        let result = validate_pattern_hints(pattern, language, &plain());
        assert!(result.ok && !result.rejected, "{name}");
        assert_eq!(result.code, None, "{name}");
    }
}

#[test]
fn rewrite_unbound_metavar_rejected_even_with_force() {
    let result = validate_rewrite_hints("console.log($A)", "$B", "typescript", &forced());
    assert!(result.rejected);
    assert_eq!(result.code, Some("REWRITE_UNBOUND_METAVARIABLE"));
}

#[test]
fn rewrite_single_to_multi_mismatch_rejected_even_with_force() {
    let result = validate_rewrite_hints("foo($ARGS)", "bar($$$ARGS)", "typescript", &forced());
    assert!(result.rejected);
    assert_eq!(result.code, Some("REWRITE_CARDINALITY_MISMATCH"));
}

#[test]
fn rewrite_multi_to_single_mismatch_rejected() {
    let result = validate_rewrite_hints("foo($$$ARGS)", "bar($ARGS)", "typescript", &plain());
    assert!(result.rejected);
    assert_eq!(result.code, Some("REWRITE_CARDINALITY_MISMATCH"));
}

#[test]
fn rewrite_happy_is_ok() {
    let result = validate_rewrite_hints(
        "console.log($MSG)",
        "logger.info($MSG)",
        "typescript",
        &plain(),
    );
    assert!(result.ok && !result.rejected);
    assert_eq!(result.code, None);
}

#[test]
fn rewrite_happy_multi_is_ok() {
    let result = validate_rewrite_hints("foo($$$ARGS)", "bar($$$ARGS)", "typescript", &plain());
    assert!(result.ok);
}

#[test]
fn rewrite_empty_replacement_is_ok() {
    let result = validate_rewrite_hints("console.log($MSG)", "", "typescript", &plain());
    assert!(result.ok);
}

#[test]
fn rewrite_propagates_pattern_hint_rejection() {
    let result = validate_rewrite_hints("\\w+", "console.log(42)", "typescript", &plain());
    assert!(result.rejected);
    assert_eq!(result.code, Some("PATTERN_HINT_REJECTED"));
}

#[test]
fn rewrite_force_bypasses_pattern_hints_but_not_unbound() {
    let result = validate_rewrite_hints("\\w+", "$B", "typescript", &forced());
    assert!(result.rejected);
    assert_eq!(result.code, Some("REWRITE_UNBOUND_METAVARIABLE"));
}

#[test]
fn rewrite_force_bypasses_pattern_hints_with_valid_rewrite() {
    let result = validate_rewrite_hints("\\w+", "console.log(42)", "typescript", &forced());
    assert!(result.ok);
}

#[test]
fn rewrite_empty_pattern_always_rejected() {
    let result = validate_rewrite_hints("", "$A", "typescript", &plain());
    assert!(result.rejected);
    assert_eq!(result.code, Some("PATTERN_EMPTY"));
}

#[test]
fn metavar_name_validation_mixed_case() {
    let cases = [
        ("$Foo rejected", "console.log($Foo)", true),
        ("$Afoo rejected", "console.log($Afoo)", true),
        ("$foo rejected", "console.log($foo)", true),
        ("$MSG accepted", "console.log($MSG)", false),
        ("$_ accepted", "console.log($_)", false),
        ("$$$ARGS accepted", "foo($$$ARGS)", false),
    ];
    for (name, pattern, should_reject) in cases {
        let result = validate_pattern_hints(pattern, "typescript", &plain());
        if should_reject {
            assert!(result.rejected, "{name}");
            assert_eq!(result.code, Some("PATTERN_HINT_REJECTED"), "{name}");
            assert!(has_hint(&result, "INVALID_METAVAR_NAME"), "{name}");
        } else {
            assert!(result.ok, "{name}");
            assert!(!has_hint(&result, "INVALID_METAVAR_NAME"), "{name}");
        }
    }
}

#[test]
fn mixed_case_metavar_force_bypasses() {
    let result = validate_pattern_hints("console.log($Foo)", "typescript", &forced());
    assert!(result.ok);
    assert!(has_hint(&result, "INVALID_METAVAR_NAME"));
}

#[test]
fn bare_pipe_between_metavars_warns() {
    let result = validate_pattern_hints("$A | $B", "typescript", &plain());
    assert!(has_hint(&result, "BARE_ALTERNATION"));
    assert!(!result.rejected);
}

#[test]
fn bare_pipe_between_calls_warns() {
    let result = validate_pattern_hints("foo() | bar()", "typescript", &plain());
    assert!(has_hint(&result, "BARE_ALTERNATION"));
    assert!(!result.rejected);
}

const INCOMPLETE_FORMS: [(&str, &str, &str); 4] = [
    ("JS function foo() no body", "function foo()", "javascript"),
    ("TS function foo() no body", "function foo()", "typescript"),
    ("Go func foo() no body", "func foo()", "go"),
    ("Rust fn foo() no body", "fn foo()", "rust"),
];

#[test]
fn incomplete_forms_with_params_rejected_without_force() {
    for (name, pattern, language) in INCOMPLETE_FORMS {
        let result = validate_pattern_hints(pattern, language, &plain());
        assert!(result.rejected, "{name}");
        assert_eq!(result.code, Some("PATTERN_HINT_REJECTED"), "{name}");
        assert!(has_hint(&result, "PATTERN_INCOMPLETE_FORM"), "{name}");
    }
}

#[test]
fn incomplete_forms_with_params_force_bypasses() {
    for (name, pattern, language) in INCOMPLETE_FORMS {
        let result = validate_pattern_hints(pattern, language, &forced());
        assert!(result.ok, "{name}");
        assert!(has_hint(&result, "PATTERN_INCOMPLETE_FORM"), "{name}");
    }
}

#[test]
fn js_function_with_body_is_accepted() {
    let result = validate_pattern_hints("function foo() { $$$ }", "javascript", &plain());
    assert!(result.ok);
    assert!(!has_hint(&result, "PATTERN_INCOMPLETE_FORM"));
}

#[test]
fn char_class_detection_breadth() {
    let cases = [
        ("[a_]", true),
        ("[^a-z]", true),
        ("[a. ]", true),
        ("[a-z]", true),
        ("[A-Z0-9_]", true),
        ("[a, b]", false),
        ("const x = [a, b]", false),
        ("const [a, b] = pair", false),
        ("obj[foo_bar]", false),
        ("type T = [A, B]", false),
        ("arr[0]", false),
        ("obj[\"key\"]", false),
    ];
    for (pattern, should_detect) in cases {
        let result = validate_pattern_hints(pattern, "typescript", &plain());
        assert_eq!(
            has_hint(&result, "REGEX_CHAR_CLASS"),
            should_detect,
            "{pattern}"
        );
        if should_detect {
            assert!(result.rejected, "{pattern}");
            assert_eq!(result.code, Some("PATTERN_HINT_REJECTED"), "{pattern}");
        }
    }
}

#[test]
fn char_class_force_bypasses() {
    let result = validate_pattern_hints("[a_]", "typescript", &forced());
    assert!(result.ok);
    assert!(has_hint(&result, "REGEX_CHAR_CLASS"));
}

#[test]
fn char_class_round3_external_caret_and_mixed_range() {
    let cases = [
        ("^[a-z]", false),
        ("^[a_]", false),
        ("[0-Z]", true),
        ("[a-z]", true),
    ];
    for (pattern, should_detect) in cases {
        let result = validate_pattern_hints(pattern, "typescript", &plain());
        assert_eq!(
            has_hint(&result, "REGEX_CHAR_CLASS"),
            should_detect,
            "{pattern}"
        );
    }
}

#[test]
fn long_pattern_does_not_crash() {
    let long_pattern = format!("console.log({})", "A".repeat(17_000));
    let result = validate_pattern_hints(&long_pattern, "typescript", &plain());
    assert!(result.ok);
}

#[test]
fn unicode_in_pattern_is_handled() {
    let result = validate_pattern_hints("console.log(\"héllo\")", "typescript", &plain());
    assert!(result.ok);
}

#[test]
fn unicode_adjacent_to_metavar_is_handled() {
    let result = validate_pattern_hints("const $X = 'café'", "typescript", &plain());
    assert!(result.ok);
}
