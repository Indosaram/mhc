use maho_ai::types::{ModelCost, ModelCostTier};
pub const PRICING_SOURCE_URL: &str = "https://commandcode.ai/docs/resources/pricing-limits";
pub const PRICING_LAST_VERIFIED: &str = "2026-08-25";
pub struct TemporaryPricing {
    pub models: &'static [&'static str],
    pub expires_on: &'static str,
    pub description: &'static str,
}
pub const TEMPORARY_PRICING: &[TemporaryPricing] = &[
    TemporaryPricing {
        models: &["claude-sonnet-5"],
        expires_on: "2026-08-31",
        description: "introductory pricing",
    },
    TemporaryPricing {
        models: &["google/gemini-3.7-flash"],
        expires_on: "2026-12-31",
        description: "50% promotional pricing",
    },
];
fn cost(
    input: impl Into<f64>,
    output: impl Into<f64>,
    cache_read: impl Into<f64>,
    cache_write: impl Into<f64>,
    tiers: Option<Vec<ModelCostTier>>,
) -> ModelCost {
    ModelCost {
        input: input.into(),
        output: output.into(),
        cache_read: cache_read.into(),
        cache_write: cache_write.into(),
        tiers,
        effective_rates: None,
        unknown: None,
    }
}
pub fn zero_model_cost() -> ModelCost {
    cost(0.0, 0.0, 0.0, 0.0, None)
}
pub fn model_cost(id: &str) -> ModelCost {
    match id {
        "poolside/laguna-s-2.1-free" => cost(0, 0, 0, 0, None),
        "stealth/ox-alpha" => cost(0, 0, 0, 0, None),
        "tencent/hy3-paid" => cost(0.14, 0.58, 0.035, 0, None),
        "moonshotai/Kimi-K3" => cost(3, 15, 0.3, 0, None),
        "moonshotai/Kimi-K2.7-Code" => cost(0.95, 4, 0.19, 0, None),
        "moonshotai/Kimi-K2.7-Code-Highspeed" => cost(1.9, 8, 0.38, 0, None),
        "moonshotai/Kimi-K2.6" => cost(0.95, 4, 0.16, 0, None),
        "moonshotai/Kimi-K2.5" => cost(0.6, 3, 0.1, 0, None),
        "zai-org/GLM-5.3" => cost(1.4, 4.4, 0.26, 0, None),
        "zai-org/GLM-5.2" => cost(1.4, 4.4, 0.26, 0, None),
        "zai-org/GLM-5.2-Fast" => cost(3, 10.25, 0.5, 0, None),
        "zai-org/GLM-5.1" => cost(1.4, 4.4, 0.26, 0, None),
        "zai-org/GLM-5" => cost(1, 3.2, 0.2, 0, None),
        "MiniMaxAI/MiniMax-M3" => cost(0.3, 1.2, 0.06, 0, None),
        "MiniMaxAI/MiniMax-M2.7" => cost(0.3, 1.2, 0.06, 0, None),
        "MiniMaxAI/MiniMax-M2.5" => cost(0.3, 1.2, 0.03, 0, None),
        "deepseek/deepseek-v4-pro" => cost(0.66, 1.98, 0.022, 0, None),
        "deepseek/deepseek-v4-flash" => cost(0.22, 0.66, 0.007, 0, None),
        "deepseek/deepseek-v4-flash-vision-exp" => cost(0.22, 0.66, 0.007, 0, None),
        "Qwen/Qwen3.8-Max" => cost(2, 6, 0.25, 2.5, None),
        "Qwen/Qwen3.8-27B" => cost(0.4, 3, 0.04, 0, None),
        "Qwen/Qwen3.7-Max" => cost(2.5, 7.5, 0.5, 3.13, None),
        "Qwen/Qwen3.7-Plus" => cost(
            0.4,
            1.6,
            0.08,
            0.5,
            Some(vec![ModelCostTier {
                input: 1.2,
                output: 4.8,
                cache_read: 0.24,
                cache_write: 1.5,
                input_tokens_above: 256000,
            }]),
        ),
        "Qwen/Qwen3.7-Flash" => cost(
            0.03,
            0.13,
            0.006,
            0.038,
            Some(vec![
                ModelCostTier {
                    input: 0.1,
                    output: 0.4,
                    cache_read: 0.02,
                    cache_write: 0.125,
                    input_tokens_above: 32000,
                },
                ModelCostTier {
                    input: 0.2,
                    output: 0.8,
                    cache_read: 0.04,
                    cache_write: 0.25,
                    input_tokens_above: 256000,
                },
            ]),
        ),
        "Qwen/Qwen3.6-Max-Preview" => cost(1.3, 7.8, 0.26, 1.63, None),
        "Qwen/Qwen3.6-Plus" => cost(0.5, 3, 0.1, 0, None),
        "stepfun/Step-3.7-Flash" => cost(0.2, 1.15, 0.04, 0, None),
        "stepfun/Step-3.5-Flash" => cost(0.1, 0.3, 0.02, 0, None),
        "xiaomi/mimo-v2.5-pro" => cost(0.435, 0.87, 0.0036, 0, None),
        "xiaomi/mimo-v2.5" => cost(0.14, 0.28, 0.0028, 0, None),
        "nvidia/nemotron-3-ultra-550b-a55b" => cost(0.6, 2.4, 0.12, 0, None),
        "sakana/fugu-ultra" => cost(5, 30, 0.5, 0, None),
        "thinkingmachines/inkling" => cost(1, 4.05, 0.17, 0, None),
        "thinkingmachines/inkling-small" => cost(0.5, 1.2, 0.1, 0, None),
        "meta/muse-spark-1.1" => cost(1.25, 4.25, 0.15, 0, None),
        "meta/muse-spark-1.2" => cost(1.25, 4.25, 0.15, 0, None),
        "meta/muse-spark-1.2-contributor" => cost(0.1, 0.2, 0.002, 0, None),
        "claude-sonnet-5" => cost(2, 10, 0.2, 2.5, None),
        "claude-sonnet-4-6" => cost(3, 15, 0.3, 3.75, None),
        "claude-fable-5" => cost(10, 50, 1, 12.5, None),
        "claude-opus-5" => cost(5, 25, 0.5, 6.25, None),
        "claude-opus-4-8" => cost(5, 25, 0.5, 6.25, None),
        "claude-opus-4-7" => cost(5, 25, 0.5, 6.25, None),
        "claude-haiku-4-5-20251001" => cost(1, 5, 0.1, 1.25, None),
        "gpt-5.6-sol" => cost(5, 30, 0.5, 6.25, None),
        "gpt-5.6-terra" => cost(2, 12, 0.2, 2.5, None),
        "gpt-5.6-luna" => cost(0.2, 1.2, 0.02, 0.25, None),
        "gpt-5.5" => cost(5, 30, 0.5, 0, None),
        "gpt-5.4" => cost(2.5, 15, 0.25, 0, None),
        "gpt-5.3-codex" => cost(2, 8, 0.5, 0, None),
        "gpt-5.4-mini" => cost(0.75, 4.5, 0.075, 0, None),
        "google/gemini-3.7-flash" => cost(0.75, 3.75, 0.075, 0.04167, None),
        "google/gemini-3.6-flash" => cost(1.5, 7.5, 0.15, 0, None),
        "google/gemini-3.5-flash" => cost(1.5, 9, 0.15, 0, None),
        "google/gemini-3.5-flash-lite" => cost(0.3, 2.5, 0.03, 0, None),
        "google/gemini-3.1-flash-lite" => cost(0.25, 1.5, 0.03, 0, None),
        "xai/grok-4.5" => cost(2, 6, 0.5, 0, None),
        "xai/grok-4.6" => cost(
            2,
            6,
            0.5,
            0,
            Some(vec![ModelCostTier {
                input: 4.0,
                output: 12.0,
                cache_read: 1.0,
                cache_write: 0.0,
                input_tokens_above: 200000,
            }]),
        ),
        _ => zero_model_cost(),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pricing_contract() {
        let z = model_cost("not-known");
        assert_eq!(
            (
                z.input,
                z.output,
                z.cache_read,
                z.cache_write,
                z.tiers,
                z.unknown
            ),
            (0.0, 0.0, 0.0, 0.0, None, None)
        );
        let q = model_cost("Qwen/Qwen3.7-Flash");
        let t = q.tiers.unwrap();
        assert_eq!(t.len(), 2);
        assert_eq!(
            (t[0].input_tokens_above, t[1].input_tokens_above),
            (32000, 256000)
        );
        let g = model_cost("xai/grok-4.6").tiers.unwrap();
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].input_tokens_above, 200000);
        let c = model_cost("claude-sonnet-5");
        assert_eq!(
            (c.input, c.output, c.cache_read, c.cache_write),
            (2.0, 10.0, 0.2, 2.5)
        );
    }
}
