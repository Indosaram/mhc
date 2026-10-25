//! The `record_agreement` tool (pin `facts-agreement-tool.ts`): the facts extractor reports a new
//! standing agreement, or a repeated correction an existing agreement already covers.
use std::collections::BTreeSet;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use maho_ext_api::{ToolDefinition, ToolError, ToolResult};
use memory_core::agreements::{AgreementExtractionRecord, parse_agreement_record};

use crate::facts_record_tool::{FactsFailureCallback, FactsRecordRunState, FactsRecordToolResult};

pub const FACTS_AGREEMENT_TOOL_NAME: &str = "record_agreement";
pub const FACTS_AGREEMENT_TOOL_LABEL: &str = "Record agreement";
pub const FACTS_AGREEMENT_TOOL_DESCRIPTION: &str = "Record that the person stated a new standing agreement, or repeated a correction an existing agreement already covers.";
const AGREEMENT_RECORD_LIMIT: usize = 32;
const AGREEMENT_RECORD_BYTES: usize = 16 * 1024;

pub fn facts_agreement_parameters() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["action", "date", "source"],
        "properties": {
            "action": {"type": "string", "enum": ["new", "reaffirm"]},
            "id": {"type": "string"},
            "applies": {"type": "string", "enum": ["global", "project"]},
            "text": {"type": "string"},
            "source": {"type":"object", "additionalProperties":false, "required":["sessionId","messageId"], "properties":{"sessionId":{"type":"string","minLength":1},"messageId":{"type":"string","minLength":1}}},
            "date": {"type": "string"}
        }
    })
}

pub struct FactsAgreementToolInput {
    pub payload: Option<serde_json::Value>,
    pub extraction_path: PathBuf,
    /// Ids of the agreements the payload showed the extractor; `reaffirm` may name only these.
    pub known_ids: BTreeSet<String>,
    pub on_failure: Option<FactsFailureCallback>,
    pub state: Option<FactsRecordRunState>,
}

#[derive(Default)]
struct Counters {
    count: usize,
    bytes: usize,
}

pub struct FactsAgreementTool {
    payload: Option<serde_json::Value>,
    active: AtomicBool,
    counters: Mutex<Counters>,
    extraction_path: PathBuf,
    known_ids: BTreeSet<String>,
    on_failure: Option<FactsFailureCallback>,
    state: Option<FactsRecordRunState>,
}

pub fn create_facts_agreement_tool(input: FactsAgreementToolInput) -> FactsAgreementTool {
    FactsAgreementTool {
        payload: input.payload,
        active: AtomicBool::new(true),
        counters: Mutex::new(Counters::default()),
        extraction_path: input.extraction_path,
        known_ids: input.known_ids,
        on_failure: input.on_failure,
        state: input.state,
    }
}

fn result(text: String, is_error: bool) -> FactsRecordToolResult {
    FactsRecordToolResult { text, is_error }
}

impl FactsAgreementTool {
    pub fn deactivate(&self) {
        self.active.store(false, Ordering::Release);
    }

    fn reject(&self, reason: &str) -> FactsRecordToolResult {
        if let Some(on_failure) = &self.on_failure {
            on_failure(reason);
        }
        result(format!("Agreement rejected: {reason}"), true)
    }

    pub fn execute(
        &self,
        _tool_call_id: &str,
        params: &serde_json::Value,
    ) -> FactsRecordToolResult {
        if !self.active.load(Ordering::Acquire)
            || self
                .state
                .as_ref()
                .is_some_and(|state| state.cancelled.load(Ordering::Acquire))
        {
            return result(
                "Agreement rejected: the facts run is no longer active".into(),
                true,
            );
        }
        // One lock covers check, reservation and append, so racing calls cannot overshoot a limit.
        let mut counters = self.counters.lock().unwrap_or_else(PoisonError::into_inner);
        if counters.count >= AGREEMENT_RECORD_LIMIT {
            return self.reject(&format!(
                "the agreement limit ({AGREEMENT_RECORD_LIMIT}) has been reached"
            ));
        }
        let record = match parse_agreement_record(params, counters.count) {
            Ok(record) => record,
            Err(error) => return self.reject(&error.message),
        };
        if let Some(payload) = &self.payload {
            if params.get("source").is_none() {
                return self.reject("source user-message identity is required");
            }
            if let Err(error) = memory_core::agreements::validate_agreement_source(&record, payload, counters.count) {
                return self.reject(&error.to_string());
            }
        }
        if let AgreementExtractionRecord::Reaffirm { id, .. } = &record
            && !self.known_ids.contains(id)
        {
            return self.reject(&format!("no agreement has id {id}; use one of the ids in the payload's agreements, or action \"new\""));
        }
        let line = match serde_json::to_string(&record) {
            Ok(json) => format!("{json}\n"),
            Err(error) => return self.reject(&error.to_string()),
        };
        if counters.bytes + line.len() > AGREEMENT_RECORD_BYTES {
            return self.reject(&format!(
                "the agreement byte limit ({AGREEMENT_RECORD_BYTES}) has been reached"
            ));
        }
        counters.count += 1;
        counters.bytes += line.len();
        let number = counters.count;
        let appended = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.extraction_path)
            .and_then(|mut file| file.write_all(line.as_bytes()).and_then(|()| file.flush()));
        if let Err(error) = appended {
            return self.reject(&error.to_string());
        }
        result(format!("Agreement recorded ({number})."), false)
    }

    pub fn to_tool_definition(self: Arc<Self>) -> ToolDefinition {
        let tool = Arc::clone(&self);
        ToolDefinition::new(
            FACTS_AGREEMENT_TOOL_NAME,
            FACTS_AGREEMENT_TOOL_DESCRIPTION,
            facts_agreement_parameters(),
            Arc::new(move |call| {
                let tool = Arc::clone(&tool);
                let params = call.params.clone();
                let call_id = call.id;
                Box::pin(async move {
                    let outcome = tool.execute(call_id, &params);
                    if outcome.is_error {
                        Err(ToolError::Message(outcome.text))
                    } else {
                        Ok(ToolResult::text(outcome.text))
                    }
                })
            }),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(path: PathBuf, known: &[&str]) -> FactsAgreementTool {
        create_facts_agreement_tool(FactsAgreementToolInput {
            payload: None,
            extraction_path: path,
            known_ids: known.iter().map(|id| (*id).to_owned()).collect(),
            on_failure: None,
            state: None,
        })
    }

    #[test]
    fn source_must_name_an_actual_user_not_lead_assistant_or_foreign_session() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agreements.jsonl");
        let payload = serde_json::json!({"entries":[{"sessionId":"session-a","lead":{"kind":"user","source_message_id":"lead"},"entries":[{"kind":"user","source_message_id":"u1"},{"kind":"assistant","source_message_id":"a1"}]}]});
        let tool = create_facts_agreement_tool(FactsAgreementToolInput { extraction_path:path.clone(), payload:Some(payload), known_ids:Default::default(), on_failure:None, state:None });
        for (message, session) in [("a1","session-a"),("lead","session-a"),("missing","session-a"),("u1","foreign")] {
            assert!(tool.execute("call", &serde_json::json!({"action":"new","applies":"global","text":"Use fixtures","date":"2026-10-01","source":{"sessionId":session,"messageId":message}})).is_error);
        }
        assert!(!path.exists());
        assert!(!tool.execute("call", &serde_json::json!({"action":"new","applies":"global","text":"Use fixtures","date":"2026-10-01","source":{"sessionId":"session-a","messageId":"u1"}})).is_error);
        let record:serde_json::Value = serde_json::from_str(std::fs::read_to_string(path).unwrap().trim()).unwrap();
        assert_eq!(record["source"]["messageId"], "u1");
    }

    #[test]
    fn a_new_agreement_is_appended_and_counted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agreements.jsonl");
        let tool = tool(path.clone(), &[]);
        let out = tool.execute("1", &serde_json::json!({"action": "new", "applies": "global", "text": "Never force push", "date": "2026-10-01"}));
        assert_eq!(
            (out.text.as_str(), out.is_error),
            ("Agreement recorded (1).", false)
        );
        assert_eq!(std::fs::read_to_string(path).unwrap().lines().count(), 1);
    }

    #[test]
    fn reaffirming_an_unknown_id_is_rejected_without_writing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agreements.jsonl");
        let tool = tool(path.clone(), &["0123456789"]);
        let out = tool.execute(
            "1",
            &serde_json::json!({"action": "reaffirm", "id": "abcdef0123", "date": "2026-10-01"}),
        );
        assert!(out.is_error);
        assert_eq!(
            out.text,
            "Agreement rejected: no agreement has id abcdef0123; use one of the ids in the payload's agreements, or action \"new\""
        );
        assert!(!path.exists());
        let ok = tool.execute(
            "2",
            &serde_json::json!({"action": "reaffirm", "id": "0123456789", "date": "2026-10-01"}),
        );
        assert_eq!(ok.text, "Agreement recorded (1).");
    }

    #[test]
    fn a_deactivated_tool_refuses() {
        let dir = tempfile::tempdir().unwrap();
        let tool = tool(dir.path().join("a.jsonl"), &[]);
        tool.deactivate();
        let out = tool.execute("1", &serde_json::json!({}));
        assert_eq!(
            out.text,
            "Agreement rejected: the facts run is no longer active"
        );
    }

    #[test]
    fn the_record_limit_stops_the_thirty_third_record() {
        let dir = tempfile::tempdir().unwrap();
        let tool = tool(dir.path().join("a.jsonl"), &[]);
        for index in 0..AGREEMENT_RECORD_LIMIT {
            let out = tool.execute("c", &serde_json::json!({"action": "new", "applies": "project", "text": format!("agreement {index}"), "date": "2026-10-01"}));
            assert!(!out.is_error, "{}", out.text);
        }
        let out = tool.execute("c", &serde_json::json!({"action": "new", "applies": "project", "text": "one more", "date": "2026-10-01"}));
        assert_eq!(
            out.text,
            "Agreement rejected: the agreement limit (32) has been reached"
        );
    }
}
