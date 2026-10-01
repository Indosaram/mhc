//! Port of senpi packages/coding-agent/src/core/provider-display-names.ts.

use std::sync::OnceLock;

use serde_json::{Map, Value};

pub fn built_in_provider_display_names() -> &'static Map<String, Value> {
    static NAMES: OnceLock<Map<String, Value>> = OnceLock::new();
    NAMES.get_or_init(|| {
        let entries: [(&str, &str); 40] = [
            ("anthropic", "Anthropic"),
            ("anthropic-subscription", "Anthropic Subscription"),
            ("amazon-bedrock", "Amazon Bedrock"),
            ("ant-ling", "Ant Ling"),
            ("azure-openai-responses", "Azure [OI] Responses"),
            ("cerebras", "Cerebras"),
            ("cloudflare-ai-gateway", "Cloudflare AI Gateway"),
            ("cloudflare-workers-ai", "Cloudflare Workers AI"),
            ("cursor", "Cursor"),
            ("cursor-cli-oauth", "Cursor CLI (OAuth)"),
            ("deepseek", "DeepSeek"),
            ("fireworks", "Fireworks"),
            ("google", "Google Gemini"),
            ("google-vertex", "Google Vertex AI"),
            ("groq", "Groq"),
            ("huggingface", "Hugging Face"),
            ("kimi-coding", "Kimi For Coding"),
            ("minimax", "MiniMax"),
            ("minimax-cn", "MiniMax (China)"),
            ("moonshotai", "Moonshot AI"),
            ("moonshotai-cn", "Moonshot AI (China)"),
            ("nvidia", "NVIDIA NIM"),
            ("opencode", "OpenCode Zen"),
            ("opencode-go", "OpenCode Go"),
            ("openai", "[OI]"),
            ("chatgpt-subscription", "ChatGPT Subscription"),
            ("opengateway", "OpenGateway"),
            ("ollama", "Ollama Cloud"),
            ("openrouter", "OpenRouter"),
            ("together", "Together AI"),
            ("venice", "Venice AI"),
            ("vercel-ai-gateway", "Vercel AI Gateway"),
            ("xai", "xAI"),
            ("zai", "ZAI Coding Plan (Global)"),
            ("zai-coding-cn", "ZAI Coding Plan (China)"),
            ("xiaomi", "Xiaomi MiMo"),
            ("xiaomi-token-plan-cn", "Xiaomi MiMo Token Plan (China)"),
            ("xiaomi-token-plan-ams", "Xiaomi MiMo Token Plan (Amsterdam)"),
            ("xiaomi-token-plan-sgp", "Xiaomi MiMo Token Plan (Singapore)"),
            ("alibaba-token-plan", "Alibaba Token Plan"),
        ];
        entries.into_iter().map(|(key, value)| (key.to_owned(), Value::String(value.to_owned()))).collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_providers_have_display_names() {
        let names = built_in_provider_display_names();
        assert_eq!(names.get("anthropic"), Some(&Value::String("Anthropic".into())));
        assert_eq!(names.get("openai"), Some(&Value::String("[OI]".into())));
        assert_eq!(names.get("xiaomi-token-plan-sgp"), Some(&Value::String("Xiaomi MiMo Token Plan (Singapore)".into())));
    }

    #[test]
    fn the_table_carries_forty_entries() {
        assert_eq!(built_in_provider_display_names().len(), 40);
    }
}
