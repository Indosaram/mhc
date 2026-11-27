//! Port of senpi `packages/coding-agent/src/modes/rpc/delegated-answer.ts`.
//!
//! Writes what the delegated-connection gate decided instead of dispatching (`senpi#5632`,
//! `senpi#4249` R-1b d1/d2). A private presenter outcome goes through the private write
//! (`write_mcp_form_reply`) and never the control queue, so it keeps the private shape's promise:
//! no event queue, snapshot, mirror or replay.

use async_trait::async_trait;
use serde_json::{Map, Value, json};

use crate::delegated_presenter_records::PRESENTER_CONTROL_INFO_COMMAND;
use crate::delegation_wire::{
    DelegationRefusal, DelegationResponse, DelegationVerdict, refused, request_id,
};

/// Adapter over the host's event writer (`SessionEventWriter` / `SessionEventFanout` +
/// `SessionMcpPresenters` + `McpPresenterControl`).
#[async_trait]
pub trait DelegationAnswerWriter: Send + Sync {
    async fn enqueue_control(&self, response: DelegationResponse) -> Result<(), String>;
    fn write_mcp_form_reply(&self, id: Option<&str>, payload: Value);
    fn handle_mcp_form(&self, record: &Value) -> bool;
    fn mcp_presenter_control_endpoint(&self) -> Option<String>;
}

/// A host-mode record whose `relayExec` is not a positive safe integer is refused, never
/// dispatched (#307).
pub fn relay_exec_refusal(record: &Value) -> DelegationResponse {
    let command = record
        .as_object()
        .and_then(|obj| obj.get("type"))
        .and_then(Value::as_str)
        .unwrap_or("parse");
    refused(
        request_id(record),
        command,
        DelegationRefusal::DelegationMalformed.as_str(),
        "relayExec must be a positive safe integer",
        None,
    )
}

pub async fn answer_delegation(
    writer: &dyn DelegationAnswerWriter,
    verdict: DelegationVerdict,
    record: &Value,
) -> Result<(), String> {
    match verdict {
        DelegationVerdict::Pass | DelegationVerdict::MachineOp { .. } => Ok(()),
        DelegationVerdict::Consumed => Ok(()),
        DelegationVerdict::Reply { response } => writer.enqueue_control(response).await,
        DelegationVerdict::Private { id, error } => {
            writer.write_mcp_form_reply(id.as_deref(), json!({ "error": error.as_str() }));
            Ok(())
        }
        DelegationVerdict::Presenter { id } => {
            // An engine without presenter enforcement has no authority to hand it to: still the private shape.
            if !writer.handle_mcp_form(record) {
                writer.write_mcp_form_reply(
                    id.as_deref(),
                    json!({ "error": "presenter_not_authorized" }),
                );
            }
            Ok(())
        }
        DelegationVerdict::ControlInfo { id } => {
            let mut data = Map::new();
            if let Some(endpoint) = writer.mcp_presenter_control_endpoint() {
                data.insert("controlEndpoint".to_owned(), Value::String(endpoint));
            }
            writer
                .enqueue_control(DelegationResponse::success(
                    id,
                    PRESENTER_CONTROL_INFO_COMMAND,
                    Value::Object(data),
                ))
                .await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::delegation_wire::PrivateRefusal;
    use std::sync::Mutex;

    #[derive(Default)]
    struct MockWriter {
        control: Mutex<Vec<DelegationResponse>>,
        private: Mutex<Vec<(Option<String>, Value)>>,
        handle_result: bool,
        endpoint: Option<String>,
    }

    #[async_trait]
    impl DelegationAnswerWriter for MockWriter {
        async fn enqueue_control(&self, response: DelegationResponse) -> Result<(), String> {
            self.control
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(response);
            Ok(())
        }

        fn write_mcp_form_reply(&self, id: Option<&str>, payload: Value) {
            self.private
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push((id.map(str::to_owned), payload));
        }

        fn handle_mcp_form(&self, _record: &Value) -> bool {
            self.handle_result
        }

        fn mcp_presenter_control_endpoint(&self) -> Option<String> {
            self.endpoint.clone()
        }
    }

    #[test]
    fn relay_exec_refusal_uses_record_type_or_parse() {
        let r1 = relay_exec_refusal(&json!({"id": "a", "type": "prompt"}));
        assert_eq!(r1.command, "prompt");
        assert_eq!(r1.id.as_deref(), Some("a"));
        assert_eq!(r1.error_code.as_deref(), Some("delegation_malformed"));

        let r2 = relay_exec_refusal(&json!(123));
        assert_eq!(r2.command, "parse");
        assert_eq!(r2.id, None);
    }

    #[tokio::test]
    async fn answers_each_verdict_on_the_right_channel() {
        let writer = MockWriter {
            handle_result: false,
            endpoint: Some("/tmp/ctrl.sock".to_owned()),
            ..Default::default()
        };

        answer_delegation(&writer, DelegationVerdict::Consumed, &json!({}))
            .await
            .expect("consumed");
        answer_delegation(
            &writer,
            DelegationVerdict::Private {
                id: Some("p1".into()),
                error: PrivateRefusal::DelegationNotGranted,
            },
            &json!({}),
        )
        .await
        .expect("private");
        answer_delegation(
            &writer,
            DelegationVerdict::Presenter {
                id: Some("p2".into()),
            },
            &json!({"type": "mcp_form_sync"}),
        )
        .await
        .expect("presenter");
        answer_delegation(
            &writer,
            DelegationVerdict::ControlInfo {
                id: Some("c1".into()),
            },
            &json!({}),
        )
        .await
        .expect("control_info");

        let private = writer
            .private
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(private.len(), 2);
        assert_eq!(private[0].1, json!({"error": "delegation_not_granted"}));
        assert_eq!(private[1].1, json!({"error": "presenter_not_authorized"}));

        let control = writer
            .control
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(control.len(), 1);
        assert_eq!(control[0].command, PRESENTER_CONTROL_INFO_COMMAND);
        assert_eq!(
            control[0].data,
            Some(json!({"controlEndpoint": "/tmp/ctrl.sock"}))
        );
    }
}
