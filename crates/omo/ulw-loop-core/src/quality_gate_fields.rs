//! Quality gate field parsing and defect collection.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use serde_json::Value;

use crate::runtime::UlwLoopError;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QualityGateDefect {
    pub field: String,
    pub message: String,
}

#[derive(Default)]
struct QualityGateCollector {
    defects: Vec<QualityGateDefect>,
    poisoned_fields: BTreeSet<String>,
    poisoned_artifact_kinds: BTreeSet<String>,
}

thread_local! {
    static ACTIVE_COLLECTOR: RefCell<Option<QualityGateCollector>> = const { RefCell::new(None) };
}

pub fn with_quality_gate_collector<T>(f: impl FnOnce() -> Result<T, UlwLoopError>) -> Result<T, UlwLoopError> {
    ACTIVE_COLLECTOR.with(|c| {
        *c.borrow_mut() = Some(QualityGateCollector::default());
    });

    let res = f();

    let collector = ACTIVE_COLLECTOR.with(|c| c.borrow_mut().take());
    if let Some(col) = collector {
        if !col.defects.is_empty() {
            throw_quality_gate_defects(&col.defects)?;
        }
    }
    res
}

pub fn throw_quality_gate_defects(defects: &[QualityGateDefect]) -> Result<(), UlwLoopError> {
    let mut unique_defects = Vec::new();
    let mut seen = BTreeSet::new();
    for d in defects {
        let key = format!("{}\0{}", d.field, d.message);
        if seen.insert(key) {
            unique_defects.push(d.clone());
        }
    }

    let fields: Vec<_> = unique_defects.iter().take(25).cloned().collect();
    let truncated = unique_defects.len() > fields.len();

    let mut lines = vec![format!(
        "Final quality gate has {}{} validation defects:",
        fields.len(),
        if truncated { "+" } else { "" }
    )];
    for item in &fields {
        lines.push(format!("- {}: {}", item.field, item.message));
    }

    let mut details = BTreeMap::new();
    if let Some(first) = fields.first() {
        details.insert("field".into(), serde_json::json!(first.field));
    }
    let fields_json: Vec<_> = fields
        .iter()
        .map(|f| serde_json::json!({ "field": f.field, "message": f.message }))
        .collect();
    details.insert("fields".into(), serde_json::json!(fields_json));
    if truncated {
        details.insert("truncated".into(), serde_json::json!(true));
    }

    Err(UlwLoopError::with_details(
        lines.join("\n"),
        "ULW_LOOP_QUALITY_GATE_INVALID",
        details,
    ))
}

pub fn invalid(message: impl Into<String>, field: impl Into<String>) {
    let msg = message.into();
    let fld = field.into();
    ACTIVE_COLLECTOR.with(|c| {
        if let Some(col) = c.borrow_mut().as_mut() {
            col.defects.push(QualityGateDefect {
                field: fld.clone(),
                message: msg,
            });
            col.poisoned_fields.insert(fld);
        }
    });
}

pub fn is_poisoned(field: &str) -> bool {
    ACTIVE_COLLECTOR.with(|c| {
        c.borrow()
            .as_ref()
            .is_some_and(|col| col.poisoned_fields.contains(field))
    })
}

pub fn mark_poisoned_artifact_kind(id: &str) {
    ACTIVE_COLLECTOR.with(|c| {
        if let Some(col) = c.borrow_mut().as_mut() {
            col.poisoned_artifact_kinds.insert(id.to_string());
        }
    });
}

pub fn is_poisoned_artifact_kind(id: &str) -> bool {
    ACTIVE_COLLECTOR.with(|c| {
        c.borrow()
            .as_ref()
            .is_some_and(|col| col.poisoned_artifact_kinds.contains(id))
    })
}

pub fn section<'a>(value: Option<&'a Value>, field: &str) -> Option<&'a serde_json::Map<String, Value>> {
    let Some(v) = value else {
        invalid(
            format!("Final quality gate is missing {field} evidence (accepted input: a bare object with manualQa/gateReview/iteration/criteriaCoverage and optional codeReview, or the same object under a top-level \"qualityGate\" key)."),
            field,
        );
        return None;
    };
    let Some(obj) = v.as_object() else {
        invalid(
            format!("Final quality gate is missing {field} evidence (accepted input: a bare object with manualQa/gateReview/iteration/criteriaCoverage and optional codeReview, or the same object under a top-level \"qualityGate\" key)."),
            field,
        );
        return None;
    };
    Some(obj)
}

fn is_placeholder(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    lower == "placeholder"
        || lower == "todo"
        || lower == "tbd"
        || lower == "n/a"
        || lower == "stub"
        || (lower.starts_with("<replace:") && lower.ends_with('>'))
}

pub fn text_field(value: Option<&Value>, field: &str) -> String {
    let Some(v) = value else {
        invalid(format!("Final quality gate requires non-empty {field}."), field);
        return String::new();
    };
    let Some(s) = v.as_str() else {
        invalid(format!("Final quality gate requires non-empty {field}."), field);
        return String::new();
    };
    let trimmed = s.trim();
    if trimmed.is_empty() {
        invalid(format!("Final quality gate requires non-empty {field}."), field);
        return String::new();
    }
    if is_placeholder(trimmed) {
        invalid(format!("Final quality gate rejects placeholder {field}."), field);
    }
    trimmed.to_string()
}

pub fn number_field(value: Option<&Value>, field: &str) -> usize {
    let Some(v) = value else {
        invalid(format!("Final quality gate requires numeric {field}."), field);
        return 0;
    };
    if let Some(n) = v.as_u64() {
        return n as usize;
    }
    if let Some(n) = v.as_f64() {
        if n.is_finite() && n >= 0.0 {
            return n as usize;
        }
    }
    invalid(format!("Final quality gate requires numeric {field}."), field);
    0
}

pub fn string_array(value: Option<&Value>, field: &str) -> Vec<String> {
    let Some(v) = value else {
        invalid(format!("Final quality gate requires {field}."), field);
        return Vec::new();
    };
    let Some(arr) = v.as_array() else {
        invalid(format!("Final quality gate requires {field}."), field);
        return Vec::new();
    };
    if arr.is_empty() {
        invalid(format!("Final quality gate requires {field}."), field);
        return Vec::new();
    }
    arr.iter().map(|item| text_field(Some(item), field)).collect()
}

pub fn string_array_allow_empty(value: Option<&Value>, field: &str) -> Vec<String> {
    let Some(v) = value else {
        invalid(format!("Final quality gate requires {field}."), field);
        return Vec::new();
    };
    let Some(arr) = v.as_array() else {
        invalid(format!("Final quality gate requires {field}."), field);
        return Vec::new();
    };
    arr.iter().map(|item| text_field(Some(item), field)).collect()
}

pub fn empty_blockers(value: Option<&Value>, field: &str) -> Vec<Value> {
    if let Some(v) = value {
        if let Some(arr) = v.as_array() {
            if arr.is_empty() {
                return Vec::new();
            }
        }
    }
    invalid(format!("{field} must be empty."), field);
    Vec::new()
}

pub fn literal_str(value: Option<&Value>, expected: &str, field: &str) -> String {
    let actual = value.and_then(|v| v.as_str()).unwrap_or("");
    if actual == expected {
        return expected.to_string();
    }
    invalid(format!("{field} must be {expected}."), field);
    expected.to_string()
}

pub fn literal_bool(value: Option<&Value>, expected: bool, field: &str) -> bool {
    let actual = value.and_then(|v| v.as_bool()).unwrap_or(!expected);
    if actual == expected {
        return expected;
    }
    invalid(format!("{field} must be {expected}."), field);
    expected
}
