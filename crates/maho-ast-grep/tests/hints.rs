//! Unit-level checks of the pattern preflight (spec §4), without any program.

use maho_ast_grep::hints::{findings, preflight, Severity};

#[test]
fn metavariable_misuse_is_a_hint() {
    assert!(preflight("$$X", "javascript", None, false).is_err());
    assert!(preflight("foo($value)", "javascript", None, false).is_err());
    assert!(preflight("foo($_)", "javascript", None, false).is_ok());
    assert!(preflight("foo($$$ARGS)", "javascript", None, false).is_ok());
    assert!(preflight("$$X", "javascript", None, true).is_ok());
}

#[test]
fn regex_syntax_is_a_hint_but_dot_star_after_a_metavariable_is_not() {
    assert!(preflight(r"\w+", "javascript", None, false).is_err());
    assert!(preflight("foo.*bar", "javascript", None, false).is_err());
    assert!(preflight("foo.+", "javascript", None, false).is_err());
    assert!(preflight("[a-z]", "javascript", None, false).is_err());
    assert!(preflight("$OBJ.*", "javascript", None, false).is_ok());
}

#[test]
fn bodiless_heads_are_hints_in_the_listed_languages() {
    assert!(preflight("fn $NAME($$$)", "rust", None, false).is_err());
    assert!(preflight("fn $NAME($$$) { $$$ }", "rust", None, false).is_ok());
    assert!(preflight("func $NAME($$$)", "go", None, false).is_err());
    assert!(preflight("function $NAME($$$)", "typescript", None, false).is_err());
    assert!(preflight("def $NAME($$$):", "python", None, false).is_err());
    assert!(preflight("def $NAME($$$)", "python", None, false).is_ok());
    assert!(preflight("fn $NAME($$$)", "java", None, false).is_ok());
}

#[test]
fn single_pipe_warns_without_rejecting() {
    let warnings = preflight("a | b", "javascript", None, false).expect("accepted");
    assert_eq!(warnings.len(), 1);
    assert!(preflight("a || b", "javascript", None, false).expect("accepted").is_empty());
    assert!(preflight("f(\"a|b\")", "javascript", None, false).expect("accepted").is_empty());
    let all = findings("a | b", "javascript", None);
    assert!(all.iter().all(|f| f.severity == Severity::Warning));
}

#[test]
fn rewrite_findings_reject_even_with_force_and_the_first_finding_is_the_message() {
    let error = preflight("old($X)", "javascript", Some("new($Y)"), true).expect_err("rejected");
    assert_eq!(error.code, "REWRITE_UNBOUND_METAVARIABLE");
    let error = preflight("old($$$X)", "javascript", Some("new($X)"), true).expect_err("rejected");
    assert_eq!(error.code, "REWRITE_METAVARIABLE_KIND_MISMATCH");
    assert!(preflight("old($X)", "javascript", Some(""), false).is_ok());
    assert!(preflight("old($X, $$$REST)", "javascript", Some("new($X, $$$REST)"), false).is_ok());
    let error = preflight("\\bx $$A", "javascript", None, false).expect_err("rejected");
    let listed = error.details["findings"].as_array().expect("findings").len();
    assert!(listed >= 2);
    assert_eq!(error.message, error.details["findings"][0]["message"].as_str().unwrap_or_default());
}
