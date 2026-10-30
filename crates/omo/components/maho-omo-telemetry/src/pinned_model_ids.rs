//! Port of `packages/omo-senpi/src/components/telemetry/pinned-model-ids.ts`.
//!
//! The collector pin's model vocabulary: sisyphuslabs/omo-telemetry 13c6e61b40c4
//! `packages/schema-registry/artifacts/omo-senpi.json`, the enum shared by `omo.model_id`,
//! `gen_ai.request.model`, `gen_ai.response.model` and the other model keys. The client vocabulary
//! (`ALL_KNOWN_MODEL_IDS`) can run ahead of the pin, and the collector rejects a whole record or span for an
//! unlisted enum value, so an unlisted id leaves as "custom". Update this list together with the pin.

pub const PINNED_MODEL_ID_LIST: [&str; 44] = [
    "claude-fable-5",
    "claude-fable-5-1",
    "claude-haiku-4-5",
    "claude-haiku-5-5",
    "claude-opus-4-6",
    "claude-opus-5",
    "claude-opus-5-5",
    "claude-sonnet-5",
    "claude-sonnet-5-5",
    "custom",
    "deepseek-flash",
    "deepseek-v4-flash",
    "deepseek-v4-pro",
    "gemini-3.1-pro",
    "gemini-3.6-flash",
    "gemini-4-argon",
    "glm-5.2",
    "glm-5.3",
    "glm-5.3-flash",
    "gpt-5.6-luna-fast",
    "gpt-5.6-sol",
    "gpt-5.6-sol-fast",
    "gpt-5.6-terra",
    "gpt-6-astra",
    "gpt-6-luna",
    "gpt-6-luna-fast",
    "gpt-6-sol",
    "gpt-6-sol-fast",
    "gpt-6.1-sol",
    "gpt-6.1-sol-fast",
    "grok-4.20-0309-non-reasoning",
    "grok-4.6",
    "grok-4.7",
    "k3",
    "kimi-for-coding-highspeed",
    "kimi-k3",
    "mimo-v2.5-pro",
    "mimo-v2.6-flash",
    "mimo-v2.6-pro",
    "minimax-m2.7",
    "minimax-m3",
    "qwen3.6-flash",
    "qwen3.7-plus",
    "qwen3.8-max-preview",
];

/// A model id the collector pin accepts: a pinned id passes through, anything else is "custom".
pub fn pinned_model_id(value: &str) -> &str {
    if PINNED_MODEL_ID_LIST.contains(&value) {
        value
    } else {
        "custom"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pinned_ids_pass_and_others_become_custom() {
        assert_eq!(pinned_model_id("claude-opus-5"), "claude-opus-5");
        assert_eq!(pinned_model_id("claude-fable-5-5"), "custom");
        assert_eq!(pinned_model_id("acme-ft"), "custom");
    }
}
