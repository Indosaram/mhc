//! Port of senpi `packages/coding-agent/src/modes/rpc/handlers-answer.ts`.
//!
//! The `answer`-class entry point: settles one `extension_ui_response` and returns its reply,
//! enforcing secret-input and grant-ownership checks before settling questions or dialogs.

use crate::extension_ui_response::answered_ui_request_id;
use serde_json::{Value, json};
use std::collections::HashMap;

pub trait SecretInputRouter {
    fn has(&self, ui_request_id: &str) -> bool;
    fn respond(&mut self, response: &Value) -> Value;
}

pub trait AnswerSettlePort {
    fn settle(&mut self, response: &Value) -> Option<Value>;
}

pub struct AnswerTargets<'a> {
    pub secret_inputs: &'a mut dyn SecretInputRouter,
    pub grant_requests: &'a HashMap<String, Option<String>>,
    pub worker_security_bridge: bool,
    pub sender: Option<&'a str>,
    pub grant_capable: bool,
    pub settle: &'a mut dyn AnswerSettlePort,
}

fn refused_reply(id: Option<&Value>, code: &str) -> Value {
    json!({
        "id": id,
        "type": "response",
        "command": "extension_ui_response",
        "success": false,
        "error": code,
        "errorCode": code,
    })
}

pub fn answer_extension_ui_response(response: &Value, targets: AnswerTargets<'_>) -> Option<Value> {
    let answered = answered_ui_request_id(response);
    let is_secret = response.get("secret").and_then(Value::as_bool) == Some(true)
        || answered.is_some_and(|id| targets.secret_inputs.has(id));
    if is_secret {
        return Some(targets.secret_inputs.respond(response));
    }
    if !targets.worker_security_bridge
        && let Some(req_id) = answered
        && let Some(expected_owner) = targets.grant_requests.get(req_id)
        && (expected_owner.as_deref() != targets.sender || !targets.grant_capable)
    {
        return Some(refused_reply(response.get("id"), "grant_answer_refused"));
    }
    targets.settle.settle(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockSecrets(bool);
    impl SecretInputRouter for MockSecrets {
        fn has(&self, id: &str) -> bool {
            self.0 && id == "sec-1"
        }
        fn respond(&mut self, r: &Value) -> Value {
            json!({"routed": "secret", "id": r["id"]})
        }
    }

    struct MockSettle;
    impl AnswerSettlePort for MockSettle {
        fn settle(&mut self, r: &Value) -> Option<Value> {
            Some(json!({"routed": "dialog", "id": r["id"]}))
        }
    }

    #[test]
    fn routes_secrets_and_enforces_grant_owner() {
        let mut secrets = MockSecrets(true);
        let mut settle = MockSettle;
        let grants = HashMap::from([("g-1".to_owned(), Some("conn-a".to_owned()))]);

        let sec = answer_extension_ui_response(
            &json!({"id":"r1","uiRequestId":"sec-1"}),
            AnswerTargets {
                secret_inputs: &mut secrets,
                grant_requests: &grants,
                worker_security_bridge: false,
                sender: Some("conn-a"),
                grant_capable: true,
                settle: &mut settle,
            },
        );
        assert_eq!(sec.expect("reply")["routed"], "secret");

        let refused = answer_extension_ui_response(
            &json!({"id":"r2","uiRequestId":"g-1"}),
            AnswerTargets {
                secret_inputs: &mut secrets,
                grant_requests: &grants,
                worker_security_bridge: false,
                sender: Some("conn-b"),
                grant_capable: true,
                settle: &mut settle,
            },
        );
        assert_eq!(refused.expect("reply")["errorCode"], "grant_answer_refused");
    }
}
