//! Admission receipts and effect records projected onto the one `EffectStatus` answer
//! (senpi `rpc/effect-status.ts`).
use crate::client_admission_record::{AdmissionState, ClientAdmissionRecord};
use crate::effect_record::{
    EffectKind, EffectOutcomeKind, EffectState, EffectStatus, RpcEffectRecord,
};

/// An input receipt as an effect: its `clientMessageId` IS its `effectId`.
pub fn project_admission(record: &ClientAdmissionRecord, session_id: Option<&str>) -> EffectStatus {
    let outcome = match &record.state {
        AdmissionState::Completed { outcome } => outcome.clone(),
        _ => None,
    };
    // A dropped or cleared input is stored as `completed` so older hosts never restore it; it never ran.
    let never_ran = outcome.as_ref().is_some_and(|outcome| {
        matches!(
            outcome.kind,
            EffectOutcomeKind::DroppedByReplace
                | EffectOutcomeKind::Cleared
                | EffectOutcomeKind::ClearedByAbort
        )
    });
    let state = if never_ran {
        EffectState::Failed
    } else {
        match record.state {
            AdmissionState::Queued { .. } => EffectState::Accepted,
            AdmissionState::Running => EffectState::Applied,
            AdmissionState::Completed { .. } => EffectState::Done,
        }
    };
    EffectStatus {
        effect_id: record.client_message_id.clone(),
        kind: EffectKind::Input,
        state,
        disposition: serde_json::to_value(record.disposition)
            .ok()
            .and_then(|value| value.as_str().map(str::to_owned)),
        outcome,
        answer: None,
        by: None,
        accepted_at: None,
        updated_at: None,
        session_id: session_id.map(str::to_owned),
        durable_session_id: record.durable_session_id.clone(),
    }
}

pub fn project_effect(record: &RpcEffectRecord, session_id: Option<&str>) -> EffectStatus {
    EffectStatus {
        effect_id: record.effect_id.clone(),
        kind: record.kind,
        state: record.state,
        disposition: record.disposition.clone(),
        outcome: record.outcome.clone(),
        answer: record.answer.clone(),
        by: Some(record.by.clone()),
        accepted_at: Some(record.accepted_at.clone()),
        updated_at: Some(record.updated_at.clone()),
        session_id: session_id.map(str::to_owned),
        durable_session_id: record.durable_session_id.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client_admission_record::parse_client_admission_record;
    use serde_json::json;

    fn record(state: serde_json::Value) -> ClientAdmissionRecord {
        let mut value = json!({"clientMessageId":"c1","durableSessionId":"d","fingerprint":"f","disposition":"queued"});
        value
            .as_object_mut()
            .expect("object")
            .extend(state.as_object().expect("object").clone());
        parse_client_admission_record(&value).expect("record")
    }

    #[test]
    fn admission_states_map_and_never_run_inputs_fail() {
        let queued = record(
            json!({"state":"queued","prepared":{"text":"t","mode":"steer","enqueueOrder":0}}),
        );
        assert_eq!(
            project_admission(&queued, Some("s")).state,
            EffectState::Accepted
        );
        assert_eq!(
            project_admission(&record(json!({"state":"running"})), None).state,
            EffectState::Applied
        );
        assert_eq!(
            project_admission(&record(json!({"state":"completed"})), None).state,
            EffectState::Done
        );
        let cleared = project_admission(
            &record(json!({"state":"completed","outcome":{"kind":"cleared_by_abort"}})),
            None,
        );
        assert_eq!(
            (cleared.state, cleared.kind, cleared.effect_id.as_str()),
            (EffectState::Failed, EffectKind::Input, "c1")
        );
        let wire = serde_json::to_value(project_admission(&queued, Some("s"))).expect("wire");
        assert_eq!(
            wire,
            json!({"effectId":"c1","kind":"input","state":"accepted","disposition":"queued","sessionId":"s","durableSessionId":"d"})
        );
    }

    #[test]
    fn effect_projection_drops_the_fingerprint() {
        let record: RpcEffectRecord = serde_json::from_value(json!({"effectId":"e","kind":"abort","state":"done","by":{"connectionId":"c"},"acceptedAt":"a","updatedAt":"u","durableSessionId":"d","fingerprint":"f"})).expect("record");
        let wire = serde_json::to_value(project_effect(&record, None)).expect("wire");
        assert!(wire.get("fingerprint").is_none());
        assert_eq!(wire["by"], json!({"connectionId":"c"}));
    }
}
