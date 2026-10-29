//! Port of `packages/omo-senpi/src/components/telemetry/omo-native-turns.ts`.
//!
//! The ONLY rounding step for cost lives here: every downstream total adds the resulting integer, so a summary
//! and the sum of its raw turns agree exactly in any summation order, and `cost_usd` can never disagree with
//! `cost_1e4`.

use std::sync::Arc;

use serde_json::{Value, json};
use telemetry_core::{TelemetryDiagnosticEvent, TelemetryDiagnostics, TelemetryError};

use crate::omo_native_parallel_summary::SummaryCapture;
use crate::omo_native_session_summary::SessionSummaryRecorder;
use crate::product_identity::mask_provider_and_model;
use crate::session_role::TelemetrySessionRole;
use crate::session_summary_window::{SummaryTurn, usd_of};

/// Rounds half-up on the decimal digits of the reported number, not on the binary product `costUsd * 10_000`:
/// `0.00015 * 1e4` is 1.4999999999999998 in binary, which a naive round takes to 1.
pub fn to_cost_1e4(cost_usd: f64) -> f64 {
    if !cost_usd.is_finite() || cost_usd < 0.0 {
        return 0.0;
    }
    let text = cost_usd.to_string();
    let (mantissa, exponent) = match text.split_once(['e', 'E']) {
        Some((mantissa, exponent)) => (mantissa, exponent.parse::<i32>().unwrap_or(0)),
        None => (text.as_str(), 0),
    };
    let (whole, fraction) = match mantissa.split_once('.') {
        Some((whole, fraction)) => (whole, fraction),
        None => (mantissa, ""),
    };
    let digits: String = format!("{whole}{fraction}");
    let point = whole.len() as i32 + exponent + 4;
    if point < 0 {
        return 0.0;
    }
    let point = point as usize;
    let mut padded = digits;
    while padded.len() < point + 1 {
        padded.push('0');
    }
    let scaled: f64 = if point == 0 {
        0.0
    } else {
        padded
            .get(..point)
            .and_then(|digits| digits.parse().ok())
            .unwrap_or(0.0)
    };
    if padded.as_bytes().get(point).is_some_and(|digit| *digit >= b'5') {
        scaled + 1.0
    } else {
        scaled
    }
}

pub struct OmoNativeTurnHandlerOptions {
    pub capture: SummaryCapture,
    pub diagnostics: Option<TelemetryDiagnostics>,
    /// The consent gate, read at send time: sampling is decided per session, long before a turn ends.
    pub is_consented: Arc<dyn Fn() -> bool + Send + Sync>,
    pub session_id: String,
    pub session_role: TelemetrySessionRole,
    pub summary: Arc<SessionSummaryRecorder>,
}

/// The `turn_completed` property set plus whether the usage was complete.
pub fn turn_completed_properties(
    event: &Value,
    session_id: &str,
    sample_rate: f64,
    session_role: TelemetrySessionRole,
) -> Option<(Value, bool, SummaryTurn)> {
    let message = event.get("message")?;
    if message.get("role").and_then(Value::as_str) != Some("assistant") {
        return None;
    }
    let usage = message.get("usage");
    let cost = usage.and_then(|usage| usage.get("cost"));
    let number = |key: &str, optional: bool| {
        let value = usage.and_then(|usage| usage.get(key));
        if optional && value.is_none() {
            return (0.0, true);
        }
        match value.and_then(Value::as_f64).filter(|n| n.is_finite() && *n >= 0.0) {
            Some(n) => (n, true),
            None => (0.0, false),
        }
    };
    let input = number("input", false);
    let output = number("output", false);
    let read = number("cacheRead", false);
    let write = number("cacheWrite", false);
    let reasoning = number("reasoning", true);
    let total = number("totalTokens", false);
    let cost_total = match cost
        .and_then(|cost| cost.get("total"))
        .and_then(Value::as_f64)
        .filter(|n| n.is_finite() && *n >= 0.0)
    {
        Some(total) => (total, true),
        None => (0.0, false),
    };
    let (provider, model_id) = mask_provider_and_model(
        message.get("provider").and_then(Value::as_str).unwrap_or_default(),
        message.get("model").and_then(Value::as_str).unwrap_or_default(),
    );
    let cost_1e4 = to_cost_1e4(cost_total.0);
    let turn_index = event.get("turnIndex").and_then(Value::as_f64);
    let stop_reason = message
        .get("stopReason")
        .and_then(Value::as_str)
        .map(str::to_string);
    let tool_results = event.get("toolResults").and_then(Value::as_array);
    let tool_result_count = tool_results.map_or(0, Vec::len) as f64;
    let tool_error_count = tool_results.map_or(0, |results| {
        results
            .iter()
            .filter(|result| result.get("isError").and_then(Value::as_bool) == Some(true))
            .count()
    }) as f64;
    let usage_valid = [input, output, read, write, reasoning, total, cost_total]
        .iter()
        .all(|(_, valid)| *valid);
    let properties = json!({
        "$session_id": session_id,
        "provider": provider,
        "model_id": model_id,
        "input_tokens": input.0,
        "output_tokens": output.0,
        "cache_read_tokens": read.0,
        "cache_write_tokens": write.0,
        "reasoning_tokens": reasoning.0,
        "total_tokens": total.0,
        "cost_1e4": cost_1e4,
        "cost_usd": usd_of(cost_1e4),
        "turn_index": turn_index,
        "sample_rate": sample_rate,
        "session_role": session_role.as_str(),
    });
    let turn = SummaryTurn {
        turn_index: turn_index.unwrap_or(0.0),
        provider,
        model_id,
        input_tokens: input.0,
        output_tokens: output.0,
        cache_read_tokens: read.0,
        cache_write_tokens: write.0,
        reasoning_tokens: reasoning.0,
        total_tokens: total.0,
        cost_1e4,
        usage_valid,
        stop_reason,
        tool_result_count,
        tool_error_count,
    };
    Some((properties, !usage_valid, turn))
}

/// Records the turn in the session's window and, for a sampled session, sends the raw `turn_completed` row.
pub async fn handle_turn_end(options: &OmoNativeTurnHandlerOptions, event: &Value) {
    let Some((properties, invalid, turn)) = turn_completed_properties(
        event,
        &options.session_id,
        options.summary.sample_rate(),
        options.session_role,
    ) else {
        return;
    };
    options
        .summary
        .record_turn(&options.session_id, turn)
        .await;
    if invalid && let Some(diagnostics) = &options.diagnostics {
        diagnostics(&telemetry_core::TelemetryDiagnosticInput {
            event: TelemetryDiagnosticEvent::TelemetryEventPropertyRejected,
            source: "omo-native-turns".into(),
            error: Some(TelemetryError::new(
                "turn_end assistant usage contained missing or invalid values",
            )),
            error_kind: Some(telemetry_core::TelemetryDiagnosticErrorKind::Error),
            entry_kind: None,
            error_code: None,
            count: None,
        });
    }
    if options.summary.is_raw_turn_sampled(&options.session_id) && (options.is_consented)() {
        (options.capture)("turn_completed", properties);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event() -> Value {
        json!({"turnIndex":7,"message":{"role":"assistant","provider":"openai","model":"gpt-5.6-sol","usage":{"input":11,"output":22,"cacheRead":3,"cacheWrite":4,"reasoning":5,"totalTokens":40,"cost":{"total":0.123456}}}})
    }

    #[test]
    fn decimal_half_up_rounding_is_exact() {
        for (cost, expected) in [
            (0.0, 0.0),
            (0.00015, 2.0),
            (0.123456, 1235.0),
            (0.000049, 0.0),
            (0.00005, 1.0),
            (0.1, 1000.0),
            (1.0, 10_000.0),
            (0.5, 5000.0),
            (0.000149, 1.0),
            (0.0001499, 1.0),
            (0.00015 + 0.0000000001, 2.0),
        ] {
            assert_eq!(to_cost_1e4(cost), expected, "{cost}");
        }
        assert_eq!(to_cost_1e4(f64::NAN), 0.0);
        assert_eq!(to_cost_1e4(f64::INFINITY), 0.0);
        assert_eq!(to_cost_1e4(-1.0), 0.0);
    }

    #[test]
    fn cost_usd_is_derived_from_the_integer() {
        for cost in [0.0, 0.0001, 0.123456, 1.5, 12.3456] {
            let integer = to_cost_1e4(cost);
            assert_eq!(usd_of(integer), integer / 10_000.0);
            assert_eq!(to_cost_1e4(usd_of(integer)), integer);
        }
    }

    #[test]
    fn model_mask_independent() {
        assert_eq!(
            mask_provider_and_model("openai", "gpt-5.6-sol"),
            ("openai".into(), "gpt-5.6-sol".into())
        );
        assert_eq!(
            mask_provider_and_model("openai", "user-defined-model"),
            ("openai".into(), "custom".into())
        );
        assert_eq!(
            mask_provider_and_model("sionic-openrouter", "gpt-5.6-sol"),
            ("custom".into(), "gpt-5.6-sol".into())
        );
        assert_eq!(
            mask_provider_and_model("sionic-openrouter", "user-defined-model"),
            ("custom".into(), "custom".into())
        );
    }

    #[test]
    fn complete_payload() {
        let (properties, invalid, turn) = turn_completed_properties(
            &event(),
            "hashed-session",
            0.05,
            TelemetrySessionRole::Root,
        )
        .unwrap();
        assert!(!invalid);
        assert_eq!(
            properties,
            json!({
                "$session_id":"hashed-session","provider":"openai","model_id":"gpt-5.6-sol",
                "input_tokens":11.0,"output_tokens":22.0,"cache_read_tokens":3.0,"cache_write_tokens":4.0,
                "reasoning_tokens":5.0,"total_tokens":40.0,"cost_1e4":1235.0,"cost_usd":0.1235,
                "turn_index":7.0,"sample_rate":0.05,"session_role":"root"
            })
        );
        assert_eq!(turn.cost_1e4, 1235.0);
        assert!(turn.usage_valid);
        assert_eq!(turn.tool_result_count, 0.0);
    }

    #[test]
    fn absent_reasoning_zero() {
        let mut event = event();
        event["message"]["usage"]
            .as_object_mut()
            .unwrap()
            .remove("reasoning");
        let (properties, invalid, _) =
            turn_completed_properties(&event, "s", 0.05, TelemetrySessionRole::Root).unwrap();
        assert_eq!(properties["reasoning_tokens"], 0.0);
        assert_eq!(properties["total_tokens"], 40.0);
        assert!(!invalid);
    }

    #[test]
    fn non_assistant_ignored() {
        assert!(
            turn_completed_properties(
                &json!({"message":{"role":"user"}}),
                "s",
                0.05,
                TelemetrySessionRole::Root
            )
            .is_none()
        );
    }

    #[test]
    fn missing_usage_zero_diagnostic() {
        let (properties, invalid, turn) = turn_completed_properties(
            &json!({"message":{"role":"assistant"}}),
            "s",
            0.05,
            TelemetrySessionRole::Root,
        )
        .unwrap();
        assert!(invalid);
        assert!(!turn.usage_valid);
        for key in [
            "input_tokens",
            "output_tokens",
            "cache_read_tokens",
            "cache_write_tokens",
            "reasoning_tokens",
            "total_tokens",
            "cost_1e4",
            "cost_usd",
        ] {
            assert_eq!(properties[key], 0.0);
        }
    }

    #[test]
    fn malformed_usage_sanitized() {
        let mut event = event();
        event["message"]["usage"] = json!({"input":null,"output":-2,"cacheRead":3,"cacheWrite":null,"reasoning":-1,"totalTokens":4,"cost":null});
        let (properties, invalid, _) =
            turn_completed_properties(&event, "s", 0.05, TelemetrySessionRole::Root).unwrap();
        assert!(invalid);
        assert_eq!(properties["input_tokens"], 0.0);
        assert_eq!(properties["output_tokens"], 0.0);
        assert_eq!(properties["cache_read_tokens"], 3.0);
        assert_eq!(properties["total_tokens"], 4.0);
    }

    #[test]
    fn tool_result_counters_reach_the_turn() {
        let mut event = event();
        event["toolResults"] = json!([{"isError":true},{"isError":false},{}]);
        let (_, _, turn) =
            turn_completed_properties(&event, "s", 0.05, TelemetrySessionRole::Child).unwrap();
        assert_eq!(turn.tool_result_count, 3.0);
        assert_eq!(turn.tool_error_count, 1.0);
    }

    #[test]
    fn a_thousand_turns_sum_to_the_same_integer_in_any_order() {
        let costs: Vec<f64> = (0..1000).map(|index| f64::from(index) / 10_000.0).collect();
        let forward: f64 = costs.iter().map(|cost| to_cost_1e4(*cost)).sum();
        let backward: f64 = costs.iter().rev().map(|cost| to_cost_1e4(*cost)).sum();
        assert_eq!(forward, backward);
        assert_eq!(forward, 499_500.0);
        assert_eq!(usd_of(forward), 49.95);
    }
}
