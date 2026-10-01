pub const KNOWN_MODELS: &[(&str, &[&str])] = &[
    ("anthropic", &["claude-fable-5", "claude-haiku-4-5", "claude-opus-5", "claude-sonnet-5"]),
    ("anthropic-api", &["claude-fable-5", "claude-haiku-4-5", "claude-opus-5", "claude-sonnet-5"]),
    ("deepseek", &["deepseek-v4-flash", "deepseek-v4-pro"]),
    ("google", &["gemini-3.6-flash"]),
    ("github-copilot", &["claude-fable-5", "claude-haiku-4-5", "claude-opus-5", "claude-sonnet-5", "gpt-5.6-sol", "gpt-5.6-terra"]),
    ("kimi-for-coding", &["k3", "kimi-for-coding-highspeed", "kimi-k3"]),
    ("moonshotai", &["kimi-k3"]),
    ("openai", &["gpt-5.6-luna-fast", "gpt-5.6-sol", "gpt-5.6-terra"]),
    ("opencode", &["claude-opus-5", "claude-sonnet-5", "gpt-5.6-sol", "kimi-k3"]),
    ("opencode-go", &["deepseek-v4-pro", "kimi-k3", "minimax-m2.7", "minimax-m3"]),
    ("quotio-openai", &["gpt-5.6-luna-fast", "gpt-5.6-sol", "gpt-5.6-terra"]),
    ("vercel", &["claude-fable-5", "claude-haiku-4-5", "claude-opus-5", "claude-sonnet-5", "deepseek-v4-flash", "deepseek-v4-pro", "gemini-3.6-flash", "gpt-5.6-sol", "gpt-5.6-terra", "kimi-k3", "minimax-m2.7", "minimax-m3"]),
    ("xai", &["grok-4.20-0309-non-reasoning"]),
];
pub fn mask_provider_and_model(provider: &str, model: &str) -> (String, String) {
    let models=KNOWN_MODELS.iter().find(|(p,_)| *p == provider).map(|(_,m)| *m);
    (if models.is_some() {provider} else {"custom"}.into(), if models.is_some_and(|m|m.contains(&model)) {model} else {"custom"}.into())
}
