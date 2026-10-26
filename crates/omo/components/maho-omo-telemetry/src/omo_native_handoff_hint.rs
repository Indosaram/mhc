//! Port of `packages/omo-senpi/src/components/telemetry/omo-native-handoff-hint.ts`.
//!
//! The handoff-hint builtin publishes a `handoff_hint` payload on the extension event bus; this subscriber
//! validates it, masks the model pair, hashes the session and rounds the money fields.

use std::sync::{Arc, Mutex, PoisonError};

use maho_ext_api::{EventKind, EventResult, ExtensionApi};
use serde_json::{Value, json};

use crate::event_schemas::HANDOFF_HINT_ACTIONS;
use crate::omo_native_parallel_summary::SummaryCapture;
use crate::product_identity::{hash_session_id, mask_provider_and_model};

/// The builtin event producer publishes this on the extension event bus.
pub const SENPI_HANDOFF_HINT_EVENT: &str = "handoff_hint";

#[derive(Debug, Clone, PartialEq)]
pub struct HandoffHintPayload {
    pub action: String,
    pub session_id: String,
    pub provider: String,
    pub model: String,
    pub context_tokens: f64,
    pub next_reply_usd: f64,
    pub fresh_usd: f64,
    pub saving_usd: f64,
    pub subscription: bool,
}

fn finite(value: Option<&Value>) -> Option<f64> {
    value.and_then(Value::as_f64).filter(|value| value.is_finite())
}

pub fn parse_payload(value: &Value) -> Option<HandoffHintPayload> {
    let action = value.get("action")?.as_str()?;
    if !HANDOFF_HINT_ACTIONS.contains(&action) {
        return None;
    }
    let session_id = value.get("sessionId")?.as_str()?;
    let provider = value.get("provider")?.as_str()?;
    let model = value.get("model")?.as_str()?;
    let context_tokens = finite(value.get("contextTokens"))?;
    let next_reply_usd = finite(value.get("nextReplyUsd"))?;
    let fresh_usd = finite(value.get("freshUsd"))?;
    let saving_usd = finite(value.get("savingUsd"))?;
    let subscription = value.get("subscription")?.as_bool()?;
    Some(HandoffHintPayload {
        action: action.to_string(),
        session_id: session_id.to_string(),
        provider: provider.to_string(),
        model: model.to_string(),
        context_tokens,
        next_reply_usd,
        fresh_usd,
        saving_usd,
        subscription,
    })
}

fn round(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

/// The exact `handoff_hint` property set, or `None` when the payload does not validate.
pub fn handoff_hint_properties(
    payload: &Value,
    hash: &(dyn Fn(&str) -> String + Send + Sync),
) -> Option<Value> {
    let event = parse_payload(payload)?;
    let (provider, model_id) = mask_provider_and_model(&event.provider, &event.model);
    Some(json!({
        "$session_id": hash(&event.session_id),
        "action": event.action,
        "context_tokens": event.context_tokens.round(),
        "fresh_usd": round(event.fresh_usd),
        "model_id": model_id,
        "next_reply_usd": round(event.next_reply_usd),
        "provider": provider,
        "saving_usd": round(event.saving_usd),
        "subscription": event.subscription,
    }))
}

pub fn register_omo_native_handoff_hint_telemetry(
    api: &mut ExtensionApi,
    capture: SummaryCapture,
    hash: Arc<dyn Fn(&str) -> String + Send + Sync>,
) {
    let subscription = api.events.on(
        SENPI_HANDOFF_HINT_EVENT,
        Arc::new(move |payload| {
            let Some(properties) = handoff_hint_properties(payload, hash.as_ref()) else {
                return;
            };
            capture("handoff_hint", properties);
        }),
    );
    let held = Arc::new(Mutex::new(Some(subscription)));
    let detached = Arc::clone(&held);
    api.on(
        EventKind::SessionShutdown,
        Arc::new(move |_, _| {
            let subscription = detached
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take();
            drop(subscription);
            Box::pin(async { Ok(EventResult::None) })
        }),
    );
}

/// The default hasher, which needs the native state dir.
pub fn session_hasher(
    state_dir: std::path::PathBuf,
) -> Arc<dyn Fn(&str) -> String + Send + Sync> {
    Arc::new(move |session_id: &str| {
        hash_session_id(session_id, &state_dir).unwrap_or_else(|error| {
            maho_ext_api::diagnostic(&format!("omo-native session identity failed: {error}"));
            String::new()
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload() -> Value {
        json!({
            "action": "shown",
            "sessionId": "raw-session",
            "provider": "openai",
            "model": "gpt-5.6-sol",
            "contextTokens": 1234.4,
            "nextReplyUsd": 0.1234,
            "freshUsd": 1.239,
            "savingUsd": 1.1156,
            "subscription": true
        })
    }

    fn hash(raw: &str) -> String {
        format!("hashed-{raw}")
    }

    fn hasher() -> impl Fn(&str) -> String {
        |raw: &str| format!("hashed-{raw}")
    }

    #[test]
    fn exact_allowlist_and_hashed_session() {
        let properties = handoff_hint_properties(&payload(), &hasher()).expect("valid payload");
        let mut keys: Vec<&str> = properties.as_object().unwrap().keys().map(String::as_str).collect();
        keys.sort_unstable();
        let mut expected: Vec<&str> =
            crate::event_schemas::OMO_NATIVE_PROPERTY_ALLOWLISTS["handoff_hint"]
                .iter()
                .map(String::as_str)
                .collect();
        expected.sort_unstable();
        assert_eq!(keys, expected);
        assert_eq!(properties["$session_id"], "hashed-raw-session");
        assert_eq!(properties["action"], "shown");
        assert_eq!(properties["context_tokens"], 1234.0);
        assert_eq!(properties["next_reply_usd"], 0.12);
        assert_eq!(properties["fresh_usd"], 1.24);
        assert_eq!(properties["saving_usd"], 1.12);
        assert_eq!(properties["provider"], "openai");
        assert_eq!(properties["model_id"], "gpt-5.6-sol");
        assert_eq!(properties["subscription"], true);
        // Upstream omo-native-handoff-hint.test.ts rejects the raw JSON value,
        // not the substring deliberately retained by its injected fake hasher.
        assert!(!properties.to_string().contains("\"raw-session\""));
    }

    #[test]
    fn the_hasher_signature_matches_the_contract() {
        let raw: &dyn Fn(&str) -> String = &hash;
        assert_eq!(raw("x"), "hashed-x");
    }

    #[test]
    fn every_action_is_accepted_and_unknown_ones_rejected() {
        for action in ["shown", "accepted", "dismissed"] {
            let mut value = payload();
            value["action"] = json!(action);
            assert_eq!(
                handoff_hint_properties(&value, &hasher()).unwrap()["action"],
                action
            );
        }
        let mut value = payload();
        value["action"] = json!("ignored");
        assert!(handoff_hint_properties(&value, &hasher()).is_none());
    }

    #[test]
    fn masking_and_validation_fail_closed() {
        let mut value = payload();
        value["provider"] = json!("private-gateway");
        value["model"] = json!("acme-internal-ft");
        let properties = handoff_hint_properties(&value, &hasher()).unwrap();
        assert_eq!(properties["provider"], "custom");
        assert_eq!(properties["model_id"], "custom");

        for (key, replacement) in [
            ("sessionId", json!(7)),
            ("provider", json!(null)),
            ("model", json!([])),
            ("contextTokens", json!("12")),
            ("nextReplyUsd", json!(null)),
            ("subscription", json!("yes")),
        ] {
            let mut value = payload();
            value[key] = replacement;
            assert!(handoff_hint_properties(&value, &hasher()).is_none(), "{key}");
        }
        let mut value = payload();
        value["savingUsd"] = json!("1.5");
        assert!(handoff_hint_properties(&value, &hasher()).is_none());
        assert!(handoff_hint_properties(&json!(null), &hasher()).is_none());
    }

    #[test]
    fn rounding_is_half_away_from_zero_at_two_places() {
        for (value, expected) in [
            (0.125, 0.13),
            (0.124, 0.12),
            (1.005, 1.0),
            (1234.4, 1234.4),
            (0.5, 0.5),
        ] {
            assert_eq!(round(value), expected, "{value}");
        }
    }
}
