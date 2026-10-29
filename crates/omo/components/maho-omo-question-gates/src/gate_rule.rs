use serde_json::Value;

/// Headers agreed with ulw-plan and the workflow-graph planners; all fit the host's 12-character limit.
pub const PINNED_GATE_HEADERS: [&str; 4] = ["Approval", "Authorize", "승인", "권한"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GateRule {
    Header,
    Label,
}

impl GateRule {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Header => "header",
            Self::Label => "label",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GateMatch<'a> {
    pub rule: GateRule,
    pub header: Option<&'a str>,
}

const GATE_HEADERS: [&str; 5] = ["approval", "authorize", "승인", "권한", "authorise"];

// Equivalent to /^(?:approve|authori[sz]e)(?![a-z])/i (ASCII case folding).
fn gate_label(label: &str) -> bool {
    ["approve", "authorise", "authorize"].iter().any(|prefix| {
        label.get(..prefix.len()).is_some_and(|start| start.eq_ignore_ascii_case(prefix))
            && !label.as_bytes().get(prefix.len()).is_some_and(u8::is_ascii_alphabetic)
    })
}

fn trimmed_string(value: Option<&Value>) -> Option<&str> {
    // ECMAScript trim includes BOM and excludes the Unicode NEL whitespace character.
    value.and_then(Value::as_str).map(|value| {
        value.trim_matches(|ch: char| matches!(ch,
            '\u{0009}'..='\u{000d}' | '\u{0020}' | '\u{00a0}' | '\u{1680}'
            | '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}'
            | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}'
        ))
    })
}

fn option_labels(question: &serde_json::Map<String, Value>) -> impl Iterator<Item = &str> {
    question.get("options").and_then(Value::as_array).into_iter().flatten()
        .filter_map(Value::as_object)
        .filter_map(|option| trimmed_string(option.get("label")))
}

/// Finds a gate by exact header first, then by an anchored option label; never reads prose.
pub fn match_gate(input: &Value) -> Option<GateMatch<'_>> {
    let questions = input.as_object()?.get("questions")?.as_array()?;
    for question in questions.iter().filter_map(Value::as_object) {
        let header = trimmed_string(question.get("header"));
        if header.is_some_and(|header| GATE_HEADERS.contains(&header.to_lowercase().as_str())) {
            return Some(GateMatch { rule: GateRule::Header, header });
        }
    }
    for question in questions.iter().filter_map(Value::as_object) {
        if option_labels(question).any(gate_label) {
            return Some(GateMatch {
                rule: GateRule::Label,
                header: trimmed_string(question.get("header")),
            });
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn header_rule_wins_across_all_questions() {
        let input = json!({"questions": [
            {"header": "Next", "options": [{"label": "Approve"}]},
            {"header": " Approval "}
        ]});
        let result = match_gate(&input);
        assert_eq!(result, Some(GateMatch { rule: GateRule::Header, header: Some("Approval") }));
    }

    #[test]
    fn labels_require_an_anchored_word_with_ascii_boundary() {
        for label in ["Approve", "APPROVE plan", "Approve, skip review", "authorise!", "Authorize1", "Approveé"] {
            let input = json!({"questions": [{"options": [{"label": label}]}]});
            let result = match_gate(&input);
            assert_eq!(result, Some(GateMatch { rule: GateRule::Label, header: None }), "{label}");
        }
        for label in ["Approved", "Approves", "Authorizer", "Pre-approve", "Do approve"] {
            let input = json!({"questions": [{"options": [{"label": label}]}]});
            let result = match_gate(&input);
            assert_eq!(result, None, "{label}");
        }
    }

    #[test]
    fn malformed_records_and_options_are_ignored() {
        for input in [Value::Null, json!([]), json!({}), json!({"questions": {}}),
            json!({"questions": [null, [], 7, {"header": 4, "options": [null, [], {"label": 7}]}]})] {
            let result = match_gate(&input);
            assert_eq!(result, None);
        }
    }

    #[test]
    fn trimming_matches_ecmascript_whitespace() {
        for (header, expected) in [("\u{feff}Approval\u{feff}", true), ("\u{0085}Approval", false)] {
            let input = json!({"questions": [{"header": header}]});
            let result = match_gate(&input);
            assert_eq!(result.is_some(), expected);
        }
    }
}
