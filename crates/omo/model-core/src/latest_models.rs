//! Current member of each family; aliases read this registry.
pub const CLAUDE_OPUS: &str = "claude-opus-5-5";
pub const CLAUDE_SONNET: &str = "claude-sonnet-5-5";
pub const CLAUDE_HAIKU: &str = "claude-haiku-5-5";
pub const LATEST_MODEL_BY_FAMILY: &[(&str, &str)] = &[
    ("claudeOpus", CLAUDE_OPUS),
    ("claudeSonnet", CLAUDE_SONNET),
    ("claudeHaiku", CLAUDE_HAIKU),
    ("claudeFable", "claude-fable-5-1"),
    ("gptAstra", "gpt-6-astra"),
    ("gptSol", "gpt-6.1-sol"),
    ("gptSolFast", "gpt-6.1-sol-fast"),
    ("gptLunaFast", "gpt-6-luna-fast"),
    ("glm", "glm-5.3"),
    ("glmFlash", "glm-5.3-flash"),
    ("kimi", "kimi-k3"),
    ("geminiPro", "gemini-4-argon"),
];
