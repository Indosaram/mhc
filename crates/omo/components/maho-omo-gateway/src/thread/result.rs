//! A delivery result read back from its row.
//!
//! Upstream counterpart: `thread/gateway/result.ts`.

use super::errors::{thread_tool_failure, ThreadToolFailure};
use super::types::{DeliveryRow, DeliveryState, GatewayDeliveryDelivery, GatewayDeliveryResult, GatewayEndpointKind};

/// How a refused row's reason maps onto the tool taxonomy.
pub fn refusal_mapping(reason: Option<&str>) -> (&'static str, &'static str) {
    match reason {
        Some("not_steerable") => ("not_steerable", "Send with delivery auto or follow_up; a steer needs a running turn."),
        Some("turn_conflict") => ("turn_conflict", "Read the target again and retry with its current turn id, or send with delivery auto."),
        Some("expired") => ("not_resumable", "The target did not take the message within its lifetime; send it again if it still matters."),
        Some("binding_closed") => ("invalid_arguments", "The binding this message was queued under was detached or rebound; re-read the binding and send again."),
        _ => ("internal_error", "Call thread_list and check the target."),
    }
}

fn error(code: &str, message: String, next_action: &str, delivery_id: &str) -> GatewayDeliveryResult {
    GatewayDeliveryResult::Error {
        error: ThreadToolFailure {
            code: super::errors::ThreadErrorCode::parse(code).unwrap_or_else(|| panic!("unknown thread error code: {code}")),
            message,
            next_action: next_action.to_owned(),
            details: Some(serde_json::json!({ "delivery_id": delivery_id })),
        },
    }
}

/// `resultFromRow`: the outcome the sender reads, read back from the target's row.
pub fn result_from_row(row: &DeliveryRow, queue_position: i64, endpoint_kind: Option<GatewayEndpointKind>, deduplicated: bool, offline: bool) -> GatewayDeliveryResult {
    let effective_mode = row.mode_effective.map(|lane| lane.as_str().to_owned()).unwrap_or_else(|| row.mode_requested.as_str().to_owned());
    let ok = |delivery: GatewayDeliveryDelivery| GatewayDeliveryResult::Ok {
        thread_id: row.target_durable_id.clone(),
        delivery_id: row.delivery_id.clone(),
        message_seq: row.seq,
        delivery,
        effective_mode: effective_mode.clone(),
        endpoint_kind,
        deduplicated,
    };
    if matches!(row.state, DeliveryState::Admitted | DeliveryState::Applied) {
        return match row.admission_kind.as_deref() {
            Some("started") => ok(GatewayDeliveryDelivery::Started { turn_id: (row.turn_epoch.unwrap_or(0)).to_string() }),
            Some("steered") => ok(GatewayDeliveryDelivery::Steered { turn_id: (row.turn_epoch.unwrap_or(0)).to_string() }),
            _ => ok(GatewayDeliveryDelivery::Queued { queue_position: queue_position.max(1) }),
        };
    }
    if row.state == DeliveryState::Refused {
        let (code, next) = refusal_mapping(row.reason.as_deref());
        return error(code, format!("Delivery {} was refused: {}.", row.delivery_id, row.reason.as_deref().unwrap_or("unknown")), next, &row.delivery_id);
    }
    if row.state == DeliveryState::Uncertain {
        return error(
            "idempotency_uncertain",
            format!("Delivery {} may or may not have reached the target.", row.delivery_id),
            "Read the target transcript before deciding whether to send again.",
            &row.delivery_id,
        );
    }
    if offline {
        return ok(GatewayDeliveryDelivery::QueuedOffline { queue_position: queue_position.max(1) });
    }
    ok(GatewayDeliveryDelivery::Queued { queue_position: queue_position.max(1) })
}

/// The next action for a store refusal code, as the engine reports it.
pub fn engine_next_action(code: &str) -> &'static str {
    match code {
        "loop_detected" => "Stop relaying here: a direct reply to the session that messaged you is a cycle. That session reads your answer with thread_read; a bound external thread gets it through thread_report, and its questions come back through thread_answer.",
        "overloaded" => "Wait before sending to this session again.",
        "queue_full" => "Wait for the target to take its queued messages, then send again.",
        "idempotency_conflict" => "Retry with a new idempotency_key.",
        "idempotency_in_progress" => "Wait for the earlier call to settle, then retry.",
        "idempotency_uncertain" => "Read the target transcript before deciding whether to send again; the gateway never resends.",
        "invalid_arguments" => "Fix the arguments and send again.",
        _ => "Call thread_list and retry after checking the target.",
    }
}

/// The failure an engine reports for a code.
pub fn engine_failure(code: &str, message: impl Into<String>, details: Option<serde_json::Value>) -> ThreadToolFailure {
    thread_tool_failure(code, message, engine_next_action(code), details)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::thread::types::{EnvelopeOrigin, GatewayDeliveryMode, StoredDeliveryEnvelope};

    fn row(state: DeliveryState, admission_kind: Option<&str>, turn_epoch: Option<i64>) -> DeliveryRow {
        DeliveryRow {
            delivery_id: "d-1".into(),
            target_durable_id: "receiver".into(),
            seq: 3,
            sender: "session:sender".into(),
            sender_turn: None,
            envelope: StoredDeliveryEnvelope {
                provenance: None,
                origin: EnvelopeOrigin::Session { session: "sender".into() },
                actor: "planner".into(),
                via: vec![],
                root_id: "root".into(),
                hop: 1,
                delivery_id: "d-1".into(),
            },
            body: "hi".into(),
            bytes: 2,
            mode_requested: GatewayDeliveryMode::Auto,
            mode_effective: None,
            expected_turn_id: None,
            state,
            reason: None,
            admitted_by: None,
            claimed_at: None,
            attempt: 0,
            admission_kind: admission_kind.map(str::to_owned),
            turn_epoch,
            root_id: "root".into(),
            hop: 1,
            created_at: 1,
            updated_at: 1,
            expires_at: 9,
            binding_id: None,
            binding_revision: None,
            actor_user_id: None,
        }
    }

    #[test]
    fn an_admitted_row_reports_its_lane() {
        match result_from_row(&row(DeliveryState::Admitted, Some("steered"), Some(7)), 1, Some(GatewayEndpointKind::RpcHost), false, false) {
            GatewayDeliveryResult::Ok { delivery, message_seq, effective_mode, .. } => {
                assert_eq!(delivery, GatewayDeliveryDelivery::Steered { turn_id: "7".into() });
                assert_eq!(message_seq, 3);
                assert_eq!(effective_mode, "auto");
            }
            other => panic!("expected ok, got {other:?}"),
        }
    }

    #[test]
    fn a_queued_row_reports_a_position_of_at_least_one() {
        match result_from_row(&row(DeliveryState::Queued, None, None), 0, None, false, true) {
            GatewayDeliveryResult::Ok { delivery, .. } => assert_eq!(delivery, GatewayDeliveryDelivery::QueuedOffline { queue_position: 1 }),
            other => panic!("expected ok, got {other:?}"),
        }
    }

    #[test]
    fn a_refused_row_maps_its_reason() {
        let mut refused = row(DeliveryState::Refused, None, None);
        refused.reason = Some("expired".into());
        match result_from_row(&refused, 0, None, false, false) {
            GatewayDeliveryResult::Error { error } => {
                assert_eq!(error.code.as_str(), "not_resumable");
                assert!(error.message.contains("was refused: expired"));
            }
            other => panic!("expected error, got {other:?}"),
        }
    }

    #[test]
    fn an_uncertain_row_is_never_resent() {
        match result_from_row(&row(DeliveryState::Uncertain, None, None), 0, None, false, false) {
            GatewayDeliveryResult::Error { error } => assert_eq!(error.code.as_str(), "idempotency_uncertain"),
            other => panic!("expected error, got {other:?}"),
        }
    }
}
