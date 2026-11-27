use maho_codemode::tool::eval_request::*;
use maho_codemode::tool::types::*;
use serde_json::{Value, json};

#[test]
fn summary_rejects_non_strings() {
    for value in [Value::Null, json!(42), json!(true), json!({})] {
        assert!(normalize_eval_summary(&value).is_none());
    }
}
#[test]
fn summary_collapses_whitespace() {
    assert_eq!(
        normalize_eval_summary(&json!(" a\n\tb   c ")),
        Some("a b c".into())
    );
}
#[test]
fn summary_rejects_blank() {
    for value in ["", "  "] {
        assert!(normalize_eval_summary(&json!(value)).is_none());
    }
}
#[test]
fn summary_preserves_long_values() {
    let text = "s".repeat(300);
    assert_eq!(normalize_eval_summary(&json!(text)), Some(text));
}
#[test]
fn run_requires_summary() {
    assert!(parse_eval_request(&json!({"language":"js","code":"print(1)"})).is_err());
}
#[test]
fn run_rejects_invalid_summary() {
    for value in [json!(""), json!("   "), json!(42)] {
        assert!(
            parse_eval_request(&json!({"language":"js","code":"print(1)","summary":value}))
                .is_err()
        );
    }
}
#[test]
fn explicit_run_requires_summary() {
    assert!(
        parse_eval_request(&json!({"action":"run","language":"js","code":"print(1)"})).is_err()
    );
}
#[test]
fn control_ignores_summary() {
    assert_eq!(
        parse_eval_request(&json!({"action":"peek","cell_id":"c","summary":42})).unwrap(),
        EvalToolRequest::Peek {
            cell_id: "c".into()
        }
    );
    assert_eq!(
        parse_eval_request(&json!({"action":"stop","cell_id":"c"})).unwrap(),
        EvalToolRequest::Stop {
            cell_id: "c".into()
        }
    );
}
#[test]
fn legacy_title_dropped() {
    let EvalToolRequest::Run(input) = parse_eval_request(
        &json!({"language":"js","code":"print(1)","summary":"kept","title":"old"}),
    )
    .unwrap() else {
        panic!("expected run");
    };
    assert!(serde_json::to_value(input).unwrap().get("title").is_none());
}
#[test]
fn run_normalizes_summary() {
    let EvalToolRequest::Run(input) =
        parse_eval_request(&json!({"language":"js","code":"1+1","summary":" a\nb "})).unwrap()
    else {
        panic!("expected run");
    };
    assert_eq!(input.summary, "a b");
}
#[test]
fn control_requires_cell_id() {
    for action in ["peek", "stop"] {
        assert!(parse_eval_request(&json!({"action":action,"cell_id":""})).is_err());
    }
}
#[test]
fn list_needs_no_other_fields() {
    assert_eq!(
        parse_eval_request(&json!({"action":"list"})).unwrap(),
        EvalToolRequest::List
    );
}
#[test]
fn invalid_action_language_code_and_behavior_rejected() {
    for value in [
        json!(null),
        json!({"action":"bad"}),
        json!({"language":"bad"}),
        json!({"language":"js"}),
        json!({"language":"js","code":"x","summary":"s","on_timeout":"bad"}),
    ] {
        assert!(parse_eval_request(&value).is_err());
    }
}
#[test]
fn timeout_defaults_follow_mode_and_override() {
    let EvalToolRequest::Run(mut input) =
        parse_eval_request(&json!({"language":"js","code":"x","summary":"s"})).unwrap()
    else {
        panic!("expected run");
    };
    assert_eq!(
        eval_timeout_behavior(&input, "tui"),
        TimeoutBehavior::Detach
    );
    assert_eq!(
        eval_timeout_behavior(&input, "print"),
        TimeoutBehavior::Error
    );
    assert_eq!(
        eval_timeout_behavior(&input, "json"),
        TimeoutBehavior::Error
    );
    input.on_timeout = Some(TimeoutBehavior::Detach);
    assert_eq!(
        eval_timeout_behavior(&input, "json"),
        TimeoutBehavior::Detach
    );
}
#[test]
fn ignores_wrong_optional_field_types() {
    let EvalToolRequest::Run(input) = parse_eval_request(
        &json!({"language":"js","code":"x","summary":"s","timeout":"5","reset":1}),
    )
    .unwrap() else {
        panic!("expected run");
    };
    assert!(input.timeout.is_none());
    assert!(input.reset.is_none());
}

#[test]
fn omitted_language_is_inferred_and_noted_while_invalid_language_fails() {
    use maho_codemode::tool::eval_language_inference::{
        EvalLanguageInferenceBasis, eval_language_inferred_note,
    };

    assert_eq!(
        eval_run_field_required_message(EvalRunField::Language, &[EvalLanguage::Js]),
        "eval run requires language: one of \"js\""
    );
    assert_eq!(
        eval_run_field_required_message(EvalRunField::Code, &[EvalLanguage::Js]),
        "eval run requires code: the cell body to execute, verbatim"
    );

    let EvalToolRequest::Run(inferred) =
        parse_eval_request(&json!({"code": "const x = 1; return x;", "summary": "inferred"}))
            .unwrap()
    else {
        panic!("expected inferred run");
    };
    assert_eq!(inferred.language, EvalLanguage::Js);
    assert_eq!(
        inferred.language_inferred,
        Some(EvalLanguageInferenceBasis::OnlyEnabledLanguage)
    );
    assert!(
        eval_language_inferred_note(
            EvalLanguage::Js,
            EvalLanguageInferenceBasis::OnlyEnabledLanguage,
        )
        .contains(": js (")
    );

    let err =
        parse_eval_request(&json!({"language": "", "code": "const x = 1;", "summary": "bad"}))
            .unwrap_err()
            .to_string();
    assert!(err.contains("Unsupported eval language"), "{err}");
}
