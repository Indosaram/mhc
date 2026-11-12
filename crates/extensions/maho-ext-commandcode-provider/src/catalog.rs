use maho_ai::types::{InputModality, ModelThinkingLevel, ThinkingLevelMap};

pub const COMMAND_CODE_CLI_VERSION: &str = "1.32.2";
pub const MODEL_INPUT_MODALITIES: &[&str] = &[
    "claude-fable-5",
    "claude-haiku-4-5-20251001",
    "claude-opus-4-7",
    "claude-opus-4-8",
    "claude-opus-5",
    "claude-sonnet-4-6",
    "claude-sonnet-5",
    "deepseek/deepseek-v4-flash-vision-exp",
    "google/gemini-3.1-flash-lite",
    "google/gemini-3.5-flash",
    "google/gemini-3.5-flash-lite",
    "google/gemini-3.6-flash",
    "google/gemini-3.7-flash",
    "gpt-5.3-codex",
    "gpt-5.4",
    "gpt-5.4-mini",
    "gpt-5.5",
    "gpt-5.6-luna",
    "gpt-5.6-sol",
    "gpt-5.6-terra",
    "meta/muse-spark-1.1",
    "meta/muse-spark-1.2",
    "meta/muse-spark-1.2-contributor",
    "MiniMaxAI/MiniMax-M3",
    "moonshotai/Kimi-K2.5",
    "moonshotai/Kimi-K2.6",
    "moonshotai/Kimi-K2.7-Code",
    "moonshotai/Kimi-K2.7-Code-Highspeed",
    "moonshotai/Kimi-K3",
    "Qwen/Qwen3.6-Plus",
    "Qwen/Qwen3.7-Flash",
    "Qwen/Qwen3.7-Plus",
    "Qwen/Qwen3.8-27B",
    "Qwen/Qwen3.8-Max",
    "sakana/fugu-ultra",
    "stealth/ox-alpha",
    "stepfun/Step-3.7-Flash",
    "thinkingmachines/inkling",
    "thinkingmachines/inkling-small",
    "xai/grok-4.5",
    "xiaomi/mimo-v2.5",
];
pub const MODEL_REASONING: &[&str] = &[
    "claude-fable-5",
    "claude-opus-4-7",
    "claude-opus-4-8",
    "claude-opus-5",
    "claude-sonnet-4-6",
    "claude-sonnet-5",
    "deepseek/deepseek-v4-flash",
    "deepseek/deepseek-v4-flash-vision-exp",
    "deepseek/deepseek-v4-pro",
    "google/gemini-3.1-flash-lite",
    "google/gemini-3.5-flash",
    "google/gemini-3.5-flash-lite",
    "google/gemini-3.6-flash",
    "google/gemini-3.7-flash",
    "gpt-5.3-codex",
    "gpt-5.4",
    "gpt-5.4-mini",
    "gpt-5.5",
    "gpt-5.6-luna",
    "gpt-5.6-sol",
    "gpt-5.6-terra",
    "meta/muse-spark-1.1",
    "meta/muse-spark-1.2",
    "meta/muse-spark-1.2-contributor",
    "MiniMaxAI/MiniMax-M3",
    "moonshotai/Kimi-K2.7-Code",
    "moonshotai/Kimi-K2.7-Code-Highspeed",
    "moonshotai/Kimi-K3",
    "nvidia/nemotron-3-ultra-550b-a55b",
    "poolside/laguna-s-2.1-free",
    "Qwen/Qwen3.6-Max-Preview",
    "Qwen/Qwen3.6-Plus",
    "Qwen/Qwen3.7-Flash",
    "Qwen/Qwen3.7-Max",
    "Qwen/Qwen3.7-Plus",
    "Qwen/Qwen3.8-27B",
    "Qwen/Qwen3.8-Max",
    "sakana/fugu-ultra",
    "stealth/ox-alpha",
    "stepfun/Step-3.5-Flash",
    "stepfun/Step-3.7-Flash",
    "tencent/hy3-paid",
    "thinkingmachines/inkling",
    "thinkingmachines/inkling-small",
    "xai/grok-4.5",
    "xai/grok-4.6",
    "zai-org/GLM-5.2",
    "zai-org/GLM-5.3",
];
pub const MODEL_EFFORTS: &[(&str, &[&str])] = &[
    ("claude-fable-5", &["low", "medium", "high", "xhigh", "max"]),
    (
        "claude-opus-4-7",
        &["low", "medium", "high", "xhigh", "max"],
    ),
    (
        "claude-opus-4-8",
        &["low", "medium", "high", "xhigh", "max"],
    ),
    ("claude-opus-5", &["low", "medium", "high", "xhigh", "max"]),
    (
        "claude-sonnet-4-6",
        &["low", "medium", "high", "xhigh", "max"],
    ),
    (
        "claude-sonnet-5",
        &["low", "medium", "high", "xhigh", "max"],
    ),
    ("deepseek/deepseek-v4-flash", &["high", "max"]),
    ("deepseek/deepseek-v4-flash-vision-exp", &["high", "max"]),
    ("deepseek/deepseek-v4-pro", &["high", "max"]),
    ("google/gemini-3.1-flash-lite", &["low", "medium", "high"]),
    ("google/gemini-3.5-flash", &["low", "medium", "high"]),
    ("google/gemini-3.5-flash-lite", &["low", "medium", "high"]),
    ("google/gemini-3.6-flash", &["low", "medium", "high"]),
    ("google/gemini-3.7-flash", &["low", "medium", "high"]),
    ("gpt-5.3-codex", &["low", "medium", "high", "xhigh"]),
    ("gpt-5.4", &["low", "medium", "high", "xhigh"]),
    ("gpt-5.4-mini", &["low", "medium", "high"]),
    ("gpt-5.5", &["low", "medium", "high", "xhigh"]),
    ("gpt-5.6-luna", &["low", "medium", "high", "xhigh", "max"]),
    ("gpt-5.6-sol", &["low", "medium", "high", "xhigh", "max"]),
    ("gpt-5.6-terra", &["low", "medium", "high", "xhigh", "max"]),
    ("Qwen/Qwen3.8-27B", &["low", "medium", "xhigh"]),
    ("Qwen/Qwen3.8-Max", &["low", "medium", "xhigh"]),
    ("sakana/fugu-ultra", &["high", "xhigh"]),
    ("stealth/ox-alpha", &["low", "high", "max"]),
    ("xai/grok-4.5", &["low", "medium", "high"]),
    ("xai/grok-4.6", &["low", "medium", "high", "xhigh"]),
    ("zai-org/GLM-5.2", &["high", "max"]),
    ("zai-org/GLM-5.3", &["low", "high", "max"]),
];
pub const MODEL_MAX_OUTPUT_TOKENS: &[(&str, u64)] = &[
    ("poolside/laguna-s-2.1-free", 32768),
    ("Qwen/Qwen3.8-27B", 32768),
    ("stealth/ox-alpha", 131072),
];

pub fn api_for_model_id(id: &str) -> &'static str {
    if id.starts_with("claude-") {
        "anthropic-messages"
    } else {
        "openai-completions"
    }
}
pub fn input_modalities_for_model(id: &str) -> Vec<InputModality> {
    if MODEL_INPUT_MODALITIES.contains(&id) {
        vec![InputModality::Text, InputModality::Image]
    } else {
        vec![InputModality::Text]
    }
}
pub fn is_reasoning_model(id: &str) -> bool {
    MODEL_REASONING.contains(&id)
}
pub fn efforts_for_model(id: &str) -> Option<&'static [&'static str]> {
    MODEL_EFFORTS
        .iter()
        .find(|(key, _)| *key == id)
        .map(|(_, efforts)| *efforts)
}
pub fn max_output_tokens_for_model(id: &str, context_length: u64) -> u64 {
    let cap = MODEL_MAX_OUTPUT_TOKENS
        .iter()
        .find(|(key, _)| *key == id)
        .map(|(_, v)| *v)
        .unwrap_or(crate::contract::DEFAULT_MAX_OUTPUT_TOKENS);
    context_length.min(cap)
}
pub fn thinking_level_map(id: &str) -> ThinkingLevelMap {
    let efforts = efforts_for_model(id);
    ModelThinkingLevel::ALL
        .into_iter()
        .filter(|level| *level != ModelThinkingLevel::Off)
        .map(|level| {
            let value = efforts
                .filter(|items| items.contains(&level.as_str()))
                .map(|_| level.as_str().to_owned());
            (level, value)
        })
        .collect()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn catalog_contract() {
        assert_eq!(api_for_model_id("claude-opus-5"), "anthropic-messages");
        assert_eq!(api_for_model_id("gpt-5.4"), "openai-completions");
        assert_eq!(
            max_output_tokens_for_model("stealth/ox-alpha", 999999),
            131072
        );
        assert_eq!(max_output_tokens_for_model("unknown", 200000), 65536);
        let m = thinking_level_map("deepseek/deepseek-v4-pro");
        assert_eq!(m.len(), 6);
        for (level, value) in m {
            assert_eq!(
                value,
                if matches!(level.as_str(), "high" | "max") {
                    Some(level.as_str().to_owned())
                } else {
                    None
                }
            );
        }
    }
}
