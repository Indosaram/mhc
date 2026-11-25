//! Current member of each family; aliases read this registry.
//! `kimiK3` is the previous Kimi generation, kept for the rungs of providers that do not serve
//! `kimi` yet (OpenCode Zen, Bailian, Firmware, Ollama Cloud, AIHubMix).
pub const CLAUDE_OPUS: &str = "claude-opus-5-5";
pub const CLAUDE_SONNET: &str = "claude-sonnet-5-5";
pub const CLAUDE_HAIKU: &str = "claude-haiku-5-5";
pub const CLAUDE_FABLE: &str = "claude-fable-5-2";
pub const GPT_ASTRA: &str = "gpt-6.2-astra";
pub const GPT_SOL: &str = "gpt-6.2-sol";
pub const GPT_SOL_FAST: &str = "gpt-6.2-sol-fast";
pub const GPT_LUNA_FAST: &str = "gpt-6-luna-fast";
pub const GLM: &str = "glm-5.3";
pub const GLM_FLASH: &str = "glm-5.3-flash";
pub const KIMI: &str = "kimi-k3.1-pro";
pub const KIMI_K3: &str = "kimi-k3";
pub const GEMINI_PRO: &str = "gemini-4-argon";

pub const LATEST_MODEL_BY_FAMILY: &[(&str, &str)] = &[
    ("claudeOpus", CLAUDE_OPUS),
    ("claudeSonnet", CLAUDE_SONNET),
    ("claudeHaiku", CLAUDE_HAIKU),
    ("claudeFable", CLAUDE_FABLE),
    ("gptAstra", GPT_ASTRA),
    ("gptSol", GPT_SOL),
    ("gptSolFast", GPT_SOL_FAST),
    ("gptLunaFast", GPT_LUNA_FAST),
    ("glm", GLM),
    ("glmFlash", GLM_FLASH),
    ("kimi", KIMI),
    ("kimiK3", KIMI_K3),
    ("geminiPro", GEMINI_PRO),
];
