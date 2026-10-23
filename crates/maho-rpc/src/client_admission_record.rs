//! Prepared-input record of the client admission store (senpi `client-admission-record.ts`).
use serde::{Deserialize, Serialize};

/// Where a queued input came from. Additive: a record written without it restores with `None`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PreparedInputSource {
    Interactive,
    Rpc,
    Extension,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PreparedInputMode {
    Steer,
    FollowUp,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreparedInput {
    pub text: String,
    pub mode: PreparedInputMode,
    pub enqueue_order: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<PreparedInputSource>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn record_without_source_restores_unknown() {
        let input: PreparedInput =
            serde_json::from_value(json!({"text":"t","mode":"steer","enqueueOrder":1}))
                .expect("legacy record");
        assert_eq!(input.source, None);
        assert!(
            serde_json::to_value(&input)
                .expect("serialize")
                .get("source")
                .is_none()
        );
    }
    #[test]
    fn recorded_source_round_trips_and_unknown_source_is_rejected() {
        let value = json!({"text":"t","mode":"followUp","enqueueOrder":2.0,"source":"extension"});
        let input: PreparedInput = serde_json::from_value(value.clone()).expect("record");
        assert_eq!(input.source, Some(PreparedInputSource::Extension));
        assert_eq!(serde_json::to_value(&input).expect("serialize"), value);
        assert!(
            serde_json::from_value::<PreparedInput>(
                json!({"text":"t","mode":"steer","enqueueOrder":1,"source":"bot"})
            )
            .is_err()
        );
    }
}
