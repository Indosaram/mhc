//! `dag-verification-context.ts`: which verification directive a delivered wake's text carries.
//!
//! A delivered wake can remain in the transcript after the session switches model or falls back, so
//! the directive is selected again at the model-call boundary from the message's custom-type
//! metadata. The scope traversal is the load-bearing part: a `omo-senpi:wake` batch carries its
//! entries BOTH as a legacy detail array and as structured `details.items`, and both must be read or
//! a nested completion silently loses its verification scope.

use senpi_task::completion::dag_verification_directive::{
    ASTRA_DAG_RUN_VERIFICATION_DIRECTIVE, ASTRA_DAG_VERIFICATION_DIRECTIVE,
    DAG_VERIFICATION_DIRECTIVE, DAG_VERIFICATION_DIRECTIVES,
};
use serde_json::Value;

/// The custom types a wake can arrive under.
pub const TASK_COMPLETION_MESSAGE_TYPE: &str = "senpi-task.completion";
pub const DAG_WAKE_MESSAGE_TYPE: &str = "omo-senpi.dag-run";
pub const WAKE_MESSAGE_TYPE: &str = "omo-senpi:wake";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DagVerificationScope {
    Node,
    Run,
}

fn is_record(value: &Value) -> bool {
    value.is_object()
}

fn is_terminal_status(details: &Value) -> bool {
    matches!(
        details.get("status").and_then(Value::as_str),
        Some("completed" | "failed" | "cancelled")
    )
}

/// `verificationScopes`: the scopes a custom message's metadata selects.
pub fn verification_scopes(message: &Value) -> Vec<DagVerificationScope> {
    let details = message.get("details");
    match message.get("customType").and_then(Value::as_str) {
        Some(TASK_COMPLETION_MESSAGE_TYPE) => match details {
            Some(Value::Array(entries))
                if entries
                    .iter()
                    .any(|entry| is_record(entry) && entry.get("dag").is_some_and(is_record)) =>
            {
                vec![DagVerificationScope::Node]
            }
            _ => Vec::new(),
        },
        Some(DAG_WAKE_MESSAGE_TYPE) => match details {
            Some(details) if is_record(details) && is_terminal_status(details) => {
                vec![DagVerificationScope::Run]
            }
            _ => Vec::new(),
        },
        Some(WAKE_MESSAGE_TYPE) => {
            let items: &[Value] = match details {
                Some(Value::Array(entries)) => entries,
                Some(details) if is_record(details) => details
                    .get("items")
                    .and_then(Value::as_array)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]),
                _ => &[],
            };
            items
                .iter()
                .filter(|entry| is_record(entry))
                .flat_map(verification_scopes)
                .collect()
        }
        _ => Vec::new(),
    }
}

/// `selectDirectives`: rewrite the directive already present at each scope, back to front.
pub fn select_directives(
    content: &str,
    scopes: &[DagVerificationScope],
    astra: bool,
) -> String {
    let mut content = content.to_string();
    let mut cursor = content.len();
    for scope in scopes.iter().rev() {
        let found = DAG_VERIFICATION_DIRECTIVES
            .iter()
            .filter_map(|directive| {
                content
                    .get(..cursor)
                    .and_then(|head| head.rfind(directive))
                    .map(|index| (index, *directive))
            })
            .max_by_key(|(index, _)| *index);
        let Some((index, directive)) = found else {
            break;
        };
        let selected = if astra {
            match scope {
                DagVerificationScope::Run => ASTRA_DAG_RUN_VERIFICATION_DIRECTIVE,
                DagVerificationScope::Node => ASTRA_DAG_VERIFICATION_DIRECTIVE,
            }
        } else {
            DAG_VERIFICATION_DIRECTIVE
        };
        content = format!(
            "{}{selected}{}",
            &content[..index],
            &content[index + directive.len()..]
        );
        cursor = index;
    }
    content
}

/// The directive a delivered payload should carry for the receiving model.
pub fn verification_directive(
    scopes: &[DagVerificationScope],
    receiver_model: Option<&str>,
) -> &'static str {
    let astra = model_core::is_gpt6_astra_model(receiver_model);
    match scopes.first() {
        Some(DagVerificationScope::Run) if astra => ASTRA_DAG_RUN_VERIFICATION_DIRECTIVE,
        Some(_) if astra => ASTRA_DAG_VERIFICATION_DIRECTIVE,
        _ => DAG_VERIFICATION_DIRECTIVE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_nested_wake_reads_structured_items_as_well_as_legacy_arrays() {
        let completion = json!({
            "customType": "senpi-task.completion",
            "details": [{"task_id": "st_1", "dag": {"run_id": "dag_1"}}],
        });
        let legacy = json!({"customType": "omo-senpi:wake", "details": [completion]});
        let structured = json!({"customType": "omo-senpi:wake", "details": {"items": [completion]}});
        assert_eq!(
            verification_scopes(&legacy),
            vec![DagVerificationScope::Node]
        );
        assert_eq!(
            verification_scopes(&structured),
            vec![DagVerificationScope::Node]
        );
    }

    #[test]
    fn a_terminal_run_wake_selects_the_run_scope() {
        let message = json!({
            "customType": "omo-senpi.dag-run",
            "details": {"runId": "dag_1", "status": "completed"},
        });
        assert_eq!(
            verification_scopes(&message),
            vec![DagVerificationScope::Run]
        );
        let paused = json!({
            "customType": "omo-senpi.dag-run",
            "details": {"runId": "dag_1", "status": "paused"},
        });
        assert!(verification_scopes(&paused).is_empty());
    }

    #[test]
    fn a_plain_completion_without_a_dag_detail_selects_nothing() {
        let message = json!({
            "customType": "senpi-task.completion",
            "details": [{"task_id": "st_1"}],
        });
        assert!(verification_scopes(&message).is_empty());
    }

    #[test]
    fn the_astra_selection_rewrites_the_directive_in_place() {
        let content = format!("summary\n\n{DAG_VERIFICATION_DIRECTIVE}");
        let selected = select_directives(
            &content,
            &[DagVerificationScope::Node],
            true,
        );
        assert!(selected.ends_with(ASTRA_DAG_VERIFICATION_DIRECTIVE));
        assert!(selected.starts_with("summary"));
        let standard = select_directives(&content, &[DagVerificationScope::Node], false);
        assert_eq!(standard, content);
    }
}
