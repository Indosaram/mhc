use model_core::CATEGORY_MODEL_REQUIREMENTS;
use pretty_assertions::assert_eq;
use crate::support::entry;

#[test]
fn visual_engineering_prioritizes_fable_opus_then_kimi() {
    assert_eq!(CATEGORY_MODEL_REQUIREMENTS["visual-engineering"].fallback_chain, vec![
        entry(&["google","google-vertex","opencode"],"gemini-4-argon",Some("low")),
        entry(&["anthropic","anthropic-api","github-copilot","opencode"],"claude-fable-5-1",Some("max")),
        entry(&["anthropic","anthropic-api","github-copilot","opencode"],"claude-opus-5-5",Some("max")),
        entry(&["kimi-for-coding","moonshotai","opencode-go","opencode"],"kimi-k3",Some("max")),
    ]);
}
#[test]
fn deep_lanes_keep_astra_high_separate_from_sol_medium() {
    assert_eq!(CATEGORY_MODEL_REQUIREMENTS["deep-high"].fallback_chain,vec![entry(&["openai","chatgpt-subscription","github-copilot","opencode"],"gpt-6-astra",Some("high"))]);
    assert_eq!(CATEGORY_MODEL_REQUIREMENTS["deep-low"].fallback_chain,vec![
        entry(&["openai","chatgpt-subscription"],"gpt-6.1-sol",Some("medium")),
        entry(&["openai","chatgpt-subscription"],"gpt-6.1-sol-fast",Some("medium")),
        entry(&["openai","chatgpt-subscription","github-copilot","opencode"],"gpt-5.6-sol",Some("medium")),
        entry(&["openai","chatgpt-subscription"],"gpt-5.6-sol-fast",Some("medium")),
    ]);
}
#[test]
fn quick_prioritizes_luna_deepseek_then_speed_tier() {
    assert_eq!(CATEGORY_MODEL_REQUIREMENTS["quick"].fallback_chain,vec![
        entry(&["openai","chatgpt-subscription"],"gpt-6-luna-fast",Some("low")),
        entry(&["anthropic","anthropic-api","github-copilot"],"claude-haiku-5-5",Some("medium")),
        entry(&["deepseek"],"deepseek-flash",Some("off")),
        entry(&["qwen-token-plan","alibaba-token-plan","bailian-coding-plan"],"qwen3.6-flash",Some("low")),
        entry(&["opencode-go"],"minimax-m3",Some("max")),
        entry(&["opencode-go"],"minimax-m2.7",Some("max")),
        entry(&["xai"],"grok-4.20-0309-non-reasoning",None),
        entry(&["zai-coding-plan"],"glm-5.3-flash",Some("low")),
        entry(&["xiaomi"],"mimo-v2.6-flash",Some("low")),
    ]);
}
#[test]
fn unspecified_low_uses_the_selected_ten_rung_chain() {
    let chain=&CATEGORY_MODEL_REQUIREMENTS["unspecified-low"].fallback_chain;
    assert!(!chain.iter().any(|e|e.model=="gpt-5.6-luna"));
    assert_eq!(*chain,vec![
        entry(&["anthropic","anthropic-api","github-copilot","opencode"],"claude-sonnet-5-5",Some("medium")),
        entry(&["xiaomi","opencode-go"],"mimo-v2.6-pro",Some("max")),
        entry(&["xai","github-copilot","opencode-go"],"grok-4.7",Some("xhigh")),
        entry(&["zai-coding-plan","opencode-go"],"glm-5.3",Some("high")),
        entry(&["openai","chatgpt-subscription"],"gpt-6.1-sol",Some("medium")),
        entry(&["openai","chatgpt-subscription","github-copilot","opencode"],"gpt-5.6-sol",Some("medium")),
        entry(&["anthropic","anthropic-api","github-copilot","opencode"],"claude-sonnet-5",Some("low")),
        entry(&["qwen-token-plan","alibaba-token-plan","qwen-token-plan-cn","alibaba-token-plan-cn"],"qwen3.8-max-preview",Some("max")),
        entry(&["deepseek","opencode-go"],"deepseek-v4-pro",Some("max")),
        entry(&["xiaomi","opencode-go"],"mimo-v2.5-pro",Some("max")),
    ]);
}
#[test]
fn high_artistry_and_writing_follow_selected_chains() {
    let providers=&["anthropic","anthropic-api","github-copilot","opencode"];
    let kimi=&["kimi-for-coding","moonshotai","opencode-go","opencode"];
    assert_eq!(CATEGORY_MODEL_REQUIREMENTS["unspecified-high"].fallback_chain,vec![entry(providers,"claude-opus-5-5",Some("medium")),entry(&["zai-coding-plan","opencode-go"],"glm-5.3",Some("max")),entry(kimi,"kimi-k3",Some("max"))]);
    assert_eq!(CATEGORY_MODEL_REQUIREMENTS["artistry"].fallback_chain,vec![entry(providers,"claude-fable-5-1",Some("max")),entry(providers,"claude-opus-5-5",Some("max")),entry(kimi,"kimi-k3",Some("max"))]);
    assert_eq!(CATEGORY_MODEL_REQUIREMENTS["writing"].fallback_chain,vec![entry(&["google","google-vertex","opencode"],"gemini-4-argon",Some("low")),entry(providers,"claude-opus-5-5",Some("low")),entry(providers,"claude-opus-4-6",Some("max"))]);
}
