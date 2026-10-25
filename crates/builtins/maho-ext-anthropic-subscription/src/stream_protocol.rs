use maho_ai::{
    model::Model,
    types::{AssistantMessage, StopReason, Usage},
};
use serde_json::Value;
pub fn map_stop_reason(reason: Option<&str>) -> StopReason {
    match reason {
        Some("tool_use") => StopReason::ToolUse,
        Some("max_tokens") => StopReason::Length,
        _ => StopReason::Stop,
    }
}
pub fn update_usage(model: &Model, output: &mut AssistantMessage, usage: &Value) {
    for (key, target) in [
        ("input_tokens", &mut output.usage.input),
        ("output_tokens", &mut output.usage.output),
        ("cache_read_input_tokens", &mut output.usage.cache_read),
        ("cache_creation_input_tokens", &mut output.usage.cache_write),
    ] {
        if let Some(value) = usage[key].as_u64() {
            *target = value;
        }
    }
    output.usage.total_tokens = output.usage.input
        + output.usage.output
        + output.usage.cache_read
        + output.usage.cache_write;
    maho_ai::models::calculate_cost(model, &mut output.usage);
}
pub fn empty_output(model: &Model, now: i64) -> AssistantMessage {
    AssistantMessage {
        thinking_level: None,
        provider_diagnostic: None,
        prompt_cache_breakpoint: None,
        prompt_cache_diagnostics: None,
        content: Vec::new(),
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        diagnostics: None,
        usage: Usage::default(),
        stop_reason: StopReason::Stop,
        stop_details: None,
        deferred: None,
        error_message: None,
        error_cause: None,
        abort_source: None,
        raw_stop_reason: None,
        end_turn: None,
        failure_source: None,
        timing: None,
        timestamp: now,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stop_reason_mapping_preserves_tool_and_length_outcomes() {
        assert_eq!(map_stop_reason(Some("tool_use")), StopReason::ToolUse);
        assert_eq!(map_stop_reason(Some("max_tokens")), StopReason::Length);
        assert_eq!(map_stop_reason(None), StopReason::Stop);
    }
    #[test]
    fn usage_updates_only_present_fields_and_uses_shared_costs() {
        let model = maho_ai::models_generated::MODELS["anthropic"]
            .values()
            .next()
            .expect("model")
            .clone();
        let mut output = empty_output(&model, 1);
        update_usage(
            &model,
            &mut output,
            &serde_json::json!({"input_tokens":10,"cache_read_input_tokens":5,"cache_creation_input_tokens":2}),
        );
        update_usage(
            &model,
            &mut output,
            &serde_json::json!({"input_tokens":null,"output_tokens":7}),
        );
        assert_eq!(output.usage.input, 10);
        assert_eq!(output.usage.total_tokens, 24);
        assert_eq!(output.timestamp, 1);
        let mut expected = output.usage;
        maho_ai::models::calculate_cost(&model, &mut expected);
        assert_eq!(output.usage.cost, expected.cost);
    }
    #[test]
    fn empty_output_keeps_unreported_error_cause_absent() {
        let model = maho_ai::models_generated::MODELS["anthropic"]
            .values()
            .next()
            .expect("model");

        let output = empty_output(model, 123);

        assert_eq!(output.error_cause, None);
        assert!(
            serde_json::to_value(output)
                .expect("message")
                .get("errorCause")
                .is_none()
        );
    }
}
