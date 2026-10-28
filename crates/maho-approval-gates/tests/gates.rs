//! Acceptance tests 1-13 of `docs/cleanroom/specs/question-gates.md` §5.

use maho_approval_gates::{
    APPROVAL_HEADERS, ApprovalGates, GateDiagnostic, GateRule, force_required, is_approval_label,
};
use maho_ext_api::{
    EventBus, EventKind, Extension, ExtensionApi, ExtensionRuntime, ExtensionSessionProfile,
    LoadedExtension, SourceInfo,
};
use serde_json::{Value, json};

fn claude_question(header: &str, labels: &[&str]) -> Value {
    json!({
        "question": "Should the deploy go ahead?",
        "header": header,
        "multiSelect": false,
        "options": labels.iter().map(|label| json!({"label": label, "description": "Pick this one"})).collect::<Vec<_>>(),
    })
}

fn claude_input(questions: Vec<Value>) -> Value {
    json!({"waitForAnswer": false, "questions": questions})
}

fn forced(tool: &str, input: &Value) -> (Value, Option<GateDiagnostic>) {
    let mut output = input.clone();
    let diagnostic = force_required(tool, &mut output);
    (output, diagnostic)
}

/// 1. Header `Authorize`, wait flag false, `required` absent or false -> `required: true`.
#[test]
fn authorize_header_forces_required_when_absent_or_false() {
    let input = claude_input(vec![claude_question("Authorize", &["Yes", "No"])]);
    let (output, diagnostic) = forced("ask_user_question", &input);
    assert_eq!(output["required"], json!(true));
    assert!(diagnostic.is_some());
    let mut expected = input.clone();
    expected["required"] = json!(true);
    assert_eq!(output, expected, "no other field changes");

    let mut explicit_false = input;
    explicit_false["required"] = json!(false);
    let (output, diagnostic) = forced("ask_user_question", &explicit_false);
    assert_eq!(output["required"], json!(true));
    assert!(diagnostic.is_some());
}

/// 2. Each listed header, padded with a tab, trailing spaces or U+FEFF, forces `required`; the
///    diagnostic records rule `header` and the trimmed header.
#[test]
fn listed_headers_match_after_js_trim() {
    for header in ["AUTHORIZE", "Authorise", "approval", "승인", "권한"] {
        for padded in [
            format!("\t{header}"),
            format!("{header}   "),
            format!("\u{FEFF}{header}"),
        ] {
            let input = claude_input(vec![claude_question(&padded, &["Yes", "No"])]);
            let (output, diagnostic) = forced("ask_user_question", &input);
            assert_eq!(output["required"], json!(true), "{padded:?}");
            assert_eq!(
                diagnostic,
                Some(GateDiagnostic {
                    tool_name: "ask_user_question".into(),
                    rule: GateRule::Header,
                    header: Some(header.into()),
                }),
                "{padded:?}"
            );
        }
    }
}

/// 3. The header rule is checked over all questions before the label rule.
#[test]
fn header_rule_beats_label_rule() {
    let input = claude_input(vec![
        claude_question("Deploy", &["Authorize deploy", "Hold"]),
        claude_question("\tAuthorize", &["Yes", "No"]),
    ]);
    let (output, diagnostic) = forced("ask_user_question", &input);
    assert_eq!(output["required"], json!(true));
    let diagnostic = diagnostic.expect("gate forced");
    assert_eq!(diagnostic.rule, GateRule::Header);
    assert_eq!(diagnostic.header.as_deref(), Some("Authorize"));
}

/// 4. `request_user_input` with a label-rule match records rule `label` and the header.
#[test]
fn request_user_input_label_match() {
    let input = json!({
        "wait_for_answer": true,
        "questions": [{
            "id": "release",
            "header": "Release",
            "question": "Ship the release?",
            "options": [
                {"label": "authorize-release", "description": "Ship it"},
                {"label": "Wait", "description": "Hold the release"},
            ],
        }],
    });
    let (output, diagnostic) = forced("request_user_input", &input);
    assert_eq!(output["required"], json!(true));
    assert_eq!(
        diagnostic,
        Some(GateDiagnostic {
            tool_name: "request_user_input".into(),
            rule: GateRule::Label,
            header: Some("Release".into()),
        })
    );
}

/// 5. Label word boundary rules.
#[test]
fn label_first_word_boundaries() {
    for label in ["approve", "Authorise now", "APPROVE: all", "approve2", "Authorizeü"] {
        assert!(is_approval_label(label), "{label:?} should match");
        let input = claude_input(vec![claude_question("Plan", &[label, "Other choice"])]);
        let (output, diagnostic) = forced("ask_user_question", &input);
        assert_eq!(output["required"], json!(true), "{label:?}");
        assert_eq!(diagnostic.map(|d| d.rule), Some(GateRule::Label), "{label:?}");
    }
    for label in ["Approval", "Approving", "Authorised", "Unapprove", "Please authorize"] {
        assert!(!is_approval_label(label), "{label:?} should not match");
        let input = claude_input(vec![claude_question("Plan", &[label, "Other choice"])]);
        let (output, diagnostic) = forced("ask_user_question", &input);
        assert_eq!(output, input, "{label:?}");
        assert!(diagnostic.is_none(), "{label:?}");
    }
}

/// 6. Prose mentioning "authorize" does not match; headers must equal a listed word exactly.
#[test]
fn prose_and_near_miss_headers_leave_input_unchanged() {
    let input = json!({
        "waitForAnswer": true,
        "questions": [{
            "question": "Do you authorize the migration? Please authorize carefully.",
            "header": "Plan check",
            "multiSelect": false,
            "options": [
                {"label": "Go ahead", "description": "Authorize the plan and approve the rollout"},
                {"label": "Stop", "description": "Do not authorize"},
            ],
        }],
    });
    let before = serde_json::to_string(&input).expect("serialize");
    let (output, diagnostic) = forced("ask_user_question", &input);
    assert_eq!(serde_json::to_string(&output).expect("serialize"), before);
    assert!(diagnostic.is_none());

    for header in ["Approval needed", "승인 요청", "Reapproval"] {
        let input = claude_input(vec![claude_question(header, &["Yes", "No"])]);
        let (output, diagnostic) = forced("ask_user_question", &input);
        assert_eq!(output, input, "{header:?}");
        assert!(diagnostic.is_none(), "{header:?}");
    }
}

/// 7. U+0085 is not trimmed, so the header does not match.
#[test]
fn next_line_character_is_not_trimmed() {
    let input = claude_input(vec![claude_question("\u{0085}Authorize", &["Yes", "No"])]);
    let (output, diagnostic) = forced("ask_user_question", &input);
    assert_eq!(output, input);
    assert!(diagnostic.is_none());
}

/// 8. Already `required: true` -> unchanged, no diagnostic.
#[test]
fn already_required_is_untouched() {
    let mut input = claude_input(vec![claude_question("Approval", &["Approve", "Reject"])]);
    input["required"] = json!(true);
    let (output, diagnostic) = forced("ask_user_question", &input);
    assert_eq!(output, input);
    assert!(diagnostic.is_none());
}

/// 9. A gate-shaped input sent to another tool is unchanged.
#[test]
fn unrelated_tool_is_untouched() {
    let mut input = claude_input(vec![claude_question("Approval", &["Approve", "Reject"])]);
    input["path"] = json!("notes.md");
    for tool in ["write", "bash", "AskUserQuestion"] {
        let (output, diagnostic) = forced(tool, &input);
        assert_eq!(output, input, "{tool}");
        assert!(diagnostic.is_none(), "{tool}");
    }
}

/// 10. Malformed inputs are unchanged and do not error.
#[test]
fn malformed_inputs_are_skipped() {
    let inputs = [
        json!(null),
        json!("text"),
        json!(0),
        json!({"questions": null}),
        json!({"questions": "x"}),
        json!({"questions": [true, "Approval", 3]}),
        json!({"questions": [{"header": 7, "options": [{"label": "Reject"}]}]}),
        json!({"questions": [{"header": "Plan", "options": [{"label": {"text": "approve"}}, "approve", false]}]}),
        json!({"questions": [{"header": "Plan", "options": "approve"}]}),
    ];
    for tool in ["ask_user_question", "request_user_input"] {
        for input in &inputs {
            let (output, diagnostic) = forced(tool, input);
            assert_eq!(&output, input, "{tool} {input}");
            assert!(diagnostic.is_none(), "{tool} {input}");
        }
    }
}

/// 11. A label match on a question without a header yields a diagnostic without a header field.
#[test]
fn label_match_without_header_has_no_header_field() {
    let input = json!({
        "waitForAnswer": true,
        "questions": [{
            "question": "Proceed?",
            "multiSelect": false,
            "options": [
                {"label": "Approve", "description": "Proceed"},
                {"label": "Reject", "description": "Stop"},
            ],
        }],
    });
    let (output, diagnostic) = forced("ask_user_question", &input);
    assert_eq!(output["required"], json!(true));
    let diagnostic = diagnostic.expect("gate forced");
    assert_eq!(diagnostic.rule, GateRule::Label);
    assert_eq!(diagnostic.header, None);
    let line: Value = serde_json::from_str(&diagnostic.line()).expect("diagnostic line is JSON");
    assert!(line.get("header").is_none());
    assert_eq!(line["rule"], json!("label"));
    assert_eq!(line["tool"], json!("ask_user_question"));
}

/// 12. The diagnostic carries none of the question text or option strings.
#[test]
fn diagnostic_omits_question_and_option_text() {
    let secret_question = "Ship build 4711 to the payments cluster?";
    let input = json!({
        "waitForAnswer": true,
        "questions": [
            {
                "question": secret_question,
                "header": "Approval",
                "multiSelect": false,
                "options": [
                    {"label": "Approve rollout-zeta", "description": "Rolls out cluster kappa"},
                    {"label": "Hold omega", "description": "Keeps sigma running"},
                ],
            },
            {
                "question": "Notify team lambda?",
                "header": "Notify",
                "multiSelect": true,
                "options": [
                    {"label": "Yes gamma", "description": "Send delta"},
                    {"label": "No epsilon", "description": "Stay quiet theta"},
                ],
            },
        ],
    });
    let (_, diagnostic) = forced("ask_user_question", &input);
    let diagnostic = diagnostic.expect("gate forced");
    let line = diagnostic.line();
    let parsed: Value = serde_json::from_str(&line).expect("diagnostic line is JSON");
    assert_eq!(parsed["rule"], json!("header"));
    assert_eq!(parsed["header"], json!("Approval"));
    for question in input["questions"].as_array().expect("questions") {
        let mut strings = vec![question["question"].as_str().expect("text")];
        for option in question["options"].as_array().expect("options") {
            strings.push(option["label"].as_str().expect("label"));
            strings.push(option["description"].as_str().expect("description"));
        }
        for text in strings {
            assert!(!line.contains(text), "diagnostic leaked {text:?}: {line}");
        }
    }
}

/// 13. Every listed header passes the `ask_user_question` schema; a longer one is rejected.
#[test]
fn listed_headers_fit_the_question_schema() {
    let tool: maho_ai::types::Tool = serde_json::from_value(json!({
        "name": "ask_user_question",
        "description": "Ask the user",
        "parameters": maho_ext_ask_user::schema::claude_params(),
    }))
    .expect("tool");
    let args = |header: &str| {
        json!({
            "waitForAnswer": true,
            "questions": [{
                "question": "Proceed?",
                "header": header,
                "multiSelect": false,
                "options": [
                    {"label": "Approve", "description": "Proceed"},
                    {"label": "Reject", "description": "Stop"},
                ],
            }],
        })
    };
    for header in APPROVAL_HEADERS {
        maho_ai::utils::validation::validate_tool_arguments_value(
            &tool,
            "ask_user_question",
            &args(header),
        )
        .unwrap_or_else(|error| panic!("{header:?} rejected: {error}"));
    }
    assert!(
        maho_ai::utils::validation::validate_tool_arguments_value(
            &tool,
            "ask_user_question",
            &args("Approval gate"),
        )
        .is_err()
    );
}

/// The extension registers exactly one `tool_call` handler and nothing else.
#[test]
fn registers_one_tool_call_handler() {
    let mut api = ExtensionApi::new(
        LoadedExtension::new("approval-gates", ".".into(), SourceInfo::default()),
        ExtensionSessionProfile::default(),
        EventBus::default(),
        ExtensionRuntime::default(),
    );
    ApprovalGates.register(&mut api);
    assert_eq!(api.registered.handlers.len(), 1);
    assert_eq!(api.registered.handlers[&EventKind::ToolCall].len(), 1);
}
