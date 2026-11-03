//! Client correlation of a session's records (senpi `client-message-events.ts`): `clientInputContext`, the record
//! kinds that carry the active client correlation (`correlatedRecord`), and `ClientMessageEvents`, which resolves a
//! turn's input metadata without changing the order of its event prefix.
//!
//! senpi keys a record's recovery identity to the record object (`carryRecovery(event, copy)`); Rust records have
//! no object identity, so every record travels with its `RecoveryCarrier` and the correlated copy keeps it
//! (`maho_core::session_recovery_identity::carry_recovery` is that copy).
use maho_core::session_recovery_identity::{RecoveryCarrier, carry_recovery};
use serde_json::{Map, Value};

tokio::task_local! {
    /// senpi `clientInputContext`: the identity of the client input whose admission is running, read by the
    /// `skill_invocation`/`command_invocation` records that input produces.
    pub static CLIENT_INPUT_CONTEXT: Value;
}

/// The identity fields of the running client input, when one is in scope (senpi `clientInputContext.getStore()`).
pub fn client_input_context() -> Option<Value> {
    CLIENT_INPUT_CONTEXT.try_with(Value::clone).ok()
}

/// The record kinds that carry the active client correlation, so one without it answers no client message.
pub fn correlated_record(record: &Value) -> bool {
    let Some(kind) = record.get("type").and_then(Value::as_str) else {
        return false;
    };
    if kind == "entry_appended" {
        return record["entry"]["type"] == "message";
    }
    matches!(
        kind,
        "turn_start" | "turn_end" | "session_abort" | "agent_end"
    ) || kind.starts_with("message_")
        || kind.starts_with("tool_execution_")
}

/// One session record and the recovery identity it carries to the host lane.
pub type CarriedRecord = (Value, Option<RecoveryCarrier>);

/// The client-facing identity of a user message (senpi
/// `clientFacingIdentity(readClientMessageIdentity(message))`): `clientMessageId`/`clientTurnId` only, a
/// host-minted id dropped.
fn message_identity(message: &Value) -> Map<String, Value> {
    let identity = maho_core::client_message_identity::read_client_message_identity(message);
    let mut fields = Map::new();
    if let Some(id) = identity.client_message_id {
        fields.insert("clientMessageId".into(), id.into());
    }
    if let Some(id) = identity.client_turn_id {
        fields.insert("clientTurnId".into(), id.into());
    }
    match crate::client_admission_record::client_facing_identity(Value::Object(fields)) {
        Value::Object(fields) => fields,
        _ => Map::new(),
    }
}

fn role(event: &Value) -> Option<&str> {
    event.get("message")?.get("role")?.as_str()
}

/// Resolve a turn's input metadata without changing the order of its event prefix.
pub struct ClientMessageEvents {
    prefix: Option<Vec<CarriedRecord>>,
    inputs: Vec<Map<String, Value>>,
    active: Vec<Map<String, Value>>,
    saw_user: bool,
    emit: Box<dyn FnMut(Value, Option<RecoveryCarrier>) + Send>,
    enabled: Box<dyn Fn() -> bool + Send>,
}

impl ClientMessageEvents {
    pub fn new(
        emit: impl FnMut(Value, Option<RecoveryCarrier>) + Send + 'static,
        enabled: impl Fn() -> bool + Send + 'static,
    ) -> Self {
        Self {
            prefix: None,
            inputs: Vec::new(),
            active: Vec::new(),
            saw_user: false,
            emit: Box::new(emit),
            enabled: Box::new(enabled),
        }
    }

    pub fn accept(&mut self, event: Value, carrier: Option<RecoveryCarrier>) {
        if !(self.enabled)() {
            (self.emit)(event, carrier);
            return;
        }
        let kind = event
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if kind == "skill_invocation" || kind == "command_invocation" {
            let mut event = event;
            if let (Some(record), Some(Value::Object(identity))) =
                (event.as_object_mut(), client_input_context())
            {
                record.extend(identity);
            }
            (self.emit)(event, carrier);
            return;
        }
        if kind == "turn_start" {
            self.flush();
            self.prefix = Some(Vec::new());
            self.inputs = Vec::new();
            self.saw_user = false;
        }
        // A turn whose input is a custom message (a trigger turn, an extension steer) answers that
        // message, not the last client input, so it must not inherit the previous correlation.
        if kind == "message_start" && role(&event) == Some("custom") && !self.saw_user {
            self.active = Vec::new();
        }
        if kind == "message_start" && role(&event) == Some("user") {
            if !self.saw_user {
                self.active = Vec::new();
            }
            self.saw_user = true;
            let identity = message_identity(&event["message"]);
            if !identity.is_empty() {
                self.inputs.push(identity);
            }
            self.active = self.inputs.clone();
            self.flush();
        }
        if (kind == "message_start" && role(&event) == Some("assistant"))
            || kind == "agent_end"
            || kind == "agent_idle"
        {
            self.flush();
        }
        match &mut self.prefix {
            Some(prefix) => prefix.push((event, carrier)),
            None => self.output(event, carrier),
        }
    }

    fn flush(&mut self) {
        let Some(prefix) = self.prefix.take() else {
            return;
        };
        if self.saw_user {
            self.active = self.inputs.clone();
        }
        for (event, carrier) in prefix {
            self.output(event, carrier);
        }
    }

    fn output(&mut self, event: Value, carrier: Option<RecoveryCarrier>) {
        let identity = if role(&event) == Some("user") {
            Some(message_identity(&event["message"]))
        } else {
            self.active.last().cloned()
        };
        let Some(identity) = identity.filter(|_| correlated_record(&event)) else {
            (self.emit)(event, carrier);
            return;
        };
        let mut copy = event;
        if let Some(record) = copy.as_object_mut() {
            record.extend(identity);
            if self.active.len() > 1 {
                record.insert(
                    "clientMessages".into(),
                    Value::Array(self.active.iter().cloned().map(Value::Object).collect()),
                );
            }
        }
        let carrier = carry_recovery(carrier.as_ref());
        (self.emit)(copy, carrier);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compact_content_store::CompactContentStoreOptions;
    use crate::session_acceptance_lane::{SessionAcceptanceLane, SessionBootstrap};
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    #[test]
    fn turn_message_tool_and_message_entries_are_correlated() {
        for kind in [
            "turn_start",
            "turn_end",
            "message_update",
            "tool_execution_end",
            "session_abort",
            "agent_end",
        ] {
            assert!(correlated_record(&json!({"type":kind})), "{kind}");
        }
        assert!(correlated_record(
            &json!({"type":"entry_appended","entry":{"type":"message"}})
        ));
        assert!(!correlated_record(
            &json!({"type":"entry_appended","entry":{"type":"label"}})
        ));
        assert!(!correlated_record(&json!({"type":"agent_start"})));
        assert!(!correlated_record(&json!({"kind":"turn_start"})));
    }

    type Seen = Arc<Mutex<Vec<CarriedRecord>>>;
    fn events(enabled: bool) -> (ClientMessageEvents, Seen) {
        let seen: Seen = Arc::default();
        let sink = seen.clone();
        let events = ClientMessageEvents::new(
            move |record, carrier| sink.lock().unwrap().push((record, carrier)),
            move || enabled,
        );
        (events, seen)
    }
    fn carrier(id: &str) -> Option<RecoveryCarrier> {
        Some(RecoveryCarrier {
            message_id: id.into(),
            epoch: "epoch".into(),
            durable_entry_id: None,
        })
    }

    #[test]
    fn a_turn_prefix_waits_for_its_user_input_and_then_carries_its_identity() {
        let (mut events, seen) = events(true);
        events.accept(json!({"type":"turn_start"}), None);
        assert!(seen.lock().unwrap().is_empty());
        events.accept(
            json!({"type":"message_start","message":{"role":"user","clientMessageId":"c-1","clientTurnId":"t-1"}}),
            None,
        );
        events.accept(
            json!({"type":"message_start","message":{"role":"assistant"}}),
            carrier("m"),
        );
        events.accept(json!({"type":"agent_start"}), None);
        let seen = seen.lock().unwrap();
        let kinds = seen
            .iter()
            .map(|(r, _)| r["type"].clone())
            .collect::<Vec<_>>();
        assert_eq!(
            kinds,
            [
                "turn_start",
                "message_start",
                "message_start",
                "agent_start"
            ]
        );
        assert_eq!(seen[0].0["clientMessageId"], "c-1");
        assert_eq!(seen[0].0["clientTurnId"], "t-1");
        assert_eq!(seen[2].0["clientMessageId"], "c-1");
        // The correlated copy keeps the original's recovery identity.
        assert_eq!(seen[2].1, carrier("m"));
        assert!(seen[3].0.get("clientMessageId").is_none());
    }

    #[test]
    fn two_inputs_in_one_turn_name_every_client_message_and_minted_ids_are_hidden() {
        let (mut events, seen) = events(true);
        events.accept(json!({"type":"turn_start"}), None);
        events.accept(
            json!({"type":"message_start","message":{"role":"user","clientMessageId":"c-1"}}),
            None,
        );
        events.accept(
            json!({"type":"message_start","message":{"role":"user","clientMessageId":"c-2"}}),
            None,
        );
        events.accept(
            json!({"type":"message_update","message":{"role":"assistant"}}),
            None,
        );
        events.accept(json!({"type":"turn_start"}), None);
        events.accept(
            json!({"type":"message_start","message":{"role":"user","clientMessageId":"host:d:1"}}),
            None,
        );
        events.accept(json!({"type":"agent_end"}), None);
        let seen = seen.lock().unwrap();
        let update = &seen[3].0;
        assert_eq!(update["clientMessageId"], "c-2");
        assert_eq!(
            update["clientMessages"],
            json!([{"clientMessageId":"c-1"},{"clientMessageId":"c-2"}])
        );
        // A host-minted id is internal: the next turn's records carry no client identity.
        for (record, _) in &seen[4..] {
            assert!(record.get("clientMessageId").is_none(), "{record}");
            assert!(record.get("clientMessages").is_none(), "{record}");
        }
    }

    #[test]
    fn a_custom_input_turn_does_not_inherit_the_previous_correlation() {
        let (mut events, seen) = events(true);
        events.accept(json!({"type":"turn_start"}), None);
        events.accept(
            json!({"type":"message_start","message":{"role":"user","clientMessageId":"c-1"}}),
            None,
        );
        events.accept(json!({"type":"turn_end"}), None);
        events.accept(json!({"type":"turn_start"}), None);
        events.accept(
            json!({"type":"message_start","message":{"role":"custom"}}),
            None,
        );
        events.accept(json!({"type":"turn_end"}), None);
        // The trigger turn's prefix waits for its input; nothing of it has reached the wire yet.
        assert_eq!(seen.lock().unwrap().len(), 3);
        events.accept(json!({"type":"agent_end"}), None);
        let seen = seen.lock().unwrap();
        assert_eq!(seen[2].0["clientMessageId"], "c-1");
        assert_eq!(seen.len(), 7);
        for (record, _) in &seen[3..] {
            assert!(record.get("clientMessageId").is_none(), "{record}");
        }
    }

    #[tokio::test]
    async fn invocation_records_take_the_running_input_identity_and_disabled_passes_through() {
        let (mut events, seen) = events(true);
        CLIENT_INPUT_CONTEXT
            .scope(json!({"clientMessageId":"c-9"}), async {
                events.accept(json!({"type":"skill_invocation","name":"s"}), None);
            })
            .await;
        events.accept(json!({"type":"command_invocation","name":"c"}), None);
        let (mut disabled, passed) = self::events(false);
        disabled.accept(json!({"type":"turn_start"}), None);
        let seen = seen.lock().unwrap();
        assert_eq!(seen[0].0["clientMessageId"], "c-9");
        assert!(seen[1].0.get("clientMessageId").is_none());
        assert_eq!(passed.lock().unwrap()[0].0, json!({"type":"turn_start"}));
    }

    // senpi test/suite/regressions/4272-midturn-correlation.test.ts (part of #4272, delta LOW-1): turn correlation
    // is not the identity of an extension's own input.
    #[tokio::test]
    async fn keeps_a_standing_client_correlation_across_identity_less_user_input_within_the_same_turn()
     {
        let root = tempfile::tempdir().expect("tempdir");
        let lane = Arc::new(Mutex::new(SessionAcceptanceLane::new(
            Some(CompactContentStoreOptions {
                dir: root.path().display().to_string(),
                ..Default::default()
            }),
            None,
        )));
        lane.lock()
            .unwrap()
            .bootstrap(SessionBootstrap::default(), None);
        type Item = (
            Value,
            Option<crate::session_acceptance_lane::AcceptedRef>,
            Option<crate::session_acceptance_lane::AcceptedCorrelation>,
        );
        let seen: Arc<Mutex<Vec<Item>>> = Arc::default();
        let (sink, accepting) = (seen.clone(), lane.clone());
        let mut events = ClientMessageEvents::new(
            move |record, carrier| {
                let mut lane = accepting.lock().unwrap();
                let accepted = lane.accept(&record, carrier.as_ref());
                let root = lane.freeze();
                lane.release(&root);
                sink.lock()
                    .unwrap()
                    .push((record, accepted, root.correlation));
            },
            || true,
        );
        let steer = json!({"role":"user","content":"extension steer","timestamp":2});
        events.accept(json!({"type":"turn_start"}), None);
        events.accept(
            json!({"type":"message_start","message":{"role":"user","content":"question","timestamp":1,"clientMessageId":"c-1"}}),
            None,
        );
        let (standing, turn_id) = {
            let seen = seen.lock().unwrap();
            let standing = seen[0].2.clone().expect("standing correlation");
            assert_eq!(standing.client_message_id, Some(json!("c-1")));
            let turn_id = seen[0].1.as_ref().and_then(|r| r.turn_id.clone());
            assert!(turn_id.is_some());
            (standing, turn_id)
        };
        events.accept(
            json!({"type":"message_start","message":steer.clone()}),
            None,
        );
        events.accept(json!({"type":"message_end","message":steer.clone()}), None);
        events.accept(
            json!({"type":"message_start","message":{"role":"assistant","content":[{"type":"text","text":"answer"}],"timestamp":3}}),
            None,
        );
        let after = {
            let mut lane = lane.lock().unwrap();
            let after = lane.freeze();
            lane.release(&after);
            after
        };
        {
            let seen = seen.lock().unwrap();
            let own = seen
                .iter()
                .find(|(record, ..)| record["message"] == steer)
                .map(|(record, ..)| record)
                .expect("own record");
            for field in ["clientMessageId", "clientTurnId", "clientMessages"] {
                assert!(own.get(field).is_none(), "{field}");
            }
            for (index, (_, accepted, correlation)) in seen.iter().enumerate() {
                let accepted = accepted.as_ref().expect("sequenced");
                assert_eq!(accepted.turn_id, turn_id);
                assert_eq!(
                    correlation.as_ref(),
                    Some(&standing),
                    "no END or restart inside this turn"
                );
                assert_eq!(accepted.sequence, index as u64 + 2);
            }
            assert_eq!(after.turn_id, turn_id);
            assert_eq!(after.correlation, Some(standing));
            assert_eq!(after.sequence, seen.len() as u64 + 1);
        }
        let mut lane = std::mem::take(&mut *lane.lock().unwrap());
        lane.retire(crate::session_acceptance_lane::RetireReason::Close);
        lane.idle().await;
    }
}
