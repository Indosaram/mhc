use model_core::AGENT_MODEL_REQUIREMENTS;
use model_core::FallbackEntry;
use pretty_assertions::assert_eq;

use crate::support::entry;
use crate::support::strings;

fn chain(agent: &str) -> &'static [FallbackEntry] {
    &AGENT_MODEL_REQUIREMENTS[agent].fallback_chain
}

fn has(entry: &FallbackEntry, provider: &str) -> bool {
    entry.providers.iter().any(|p| p == provider)
}

#[test]
fn oracle_has_gpt_5_6_sol_xhigh_as_primary() {
    let oracle = chain("oracle");

    assert!(!oracle.is_empty());
    assert_eq!(
        oracle[0],
        entry(
            &["openai", "opencode", "vercel"],
            "gpt-5.6-sol",
            Some("xhigh")
        )
    );
    assert_eq!(
        oracle[1],
        entry(&["github-copilot"], "gpt-5.6-sol", Some("high"))
    );
}

#[test]
fn sisyphus_keeps_opus_primary_before_kimi_k3_sol_glm_5_2_and_big_pickle_fallbacks() {
    let sisyphus = &AGENT_MODEL_REQUIREMENTS["sisyphus"];
    let chain = &sisyphus.fallback_chain;

    assert_eq!(chain.len(), 5);
    assert_eq!(sisyphus.requires_any_model, Some(true));
    assert_eq!(
        chain[0],
        entry(
            &["anthropic", "github-copilot", "opencode", "vercel"],
            "claude-opus-5",
            Some("max")
        )
    );
    assert_eq!(
        chain[1],
        entry(
            &[
                "opencode-go",
                "kimi-for-coding",
                "moonshotai",
                "opencode",
                "vercel",
                "bailian-coding-plan",
                "moonshotai-cn",
                "firmware",
                "ollama-cloud",
                "aihubmix",
            ],
            "kimi-k3",
            None
        )
    );
    assert_eq!(
        chain[2],
        entry(
            &["openai", "github-copilot", "opencode", "vercel"],
            "gpt-5.6-sol",
            Some("medium")
        )
    );
    assert_eq!(chain[3].providers[0], "zai-coding-plan");
    assert_eq!(chain[3].model, "glm-5.2");
    assert_eq!(chain[4].providers[0], "opencode");
    assert_eq!(chain[4].model, "big-pickle");
}

fn assert_fast_openai_primary_chain(agent: &str) {
    let chain = chain(agent);
    // TS destructures [primary, , second, third, ...]: index 1 (DeepSeek) is skipped.
    assert_eq!(chain.len(), 9, "{agent}");
    assert_eq!(
        chain[0],
        entry(&["openai"], "gpt-5.6-luna-fast", Some("low")),
        "{agent}"
    );
    assert!(
        has(&chain[2], "opencode-go") && has(&chain[2], "bailian-coding-plan"),
        "{agent}"
    );
    assert_eq!(chain[2].model, "qwen3.7-plus", "{agent}");
    assert_eq!(
        chain[3],
        entry(&["vercel"], "minimax-m2.7-highspeed", None),
        "{agent}"
    );
    assert!(has(&chain[4], "opencode-go"), "{agent}");
    assert_eq!(chain[4].model, "minimax-m3", "{agent}");
    assert_eq!(
        chain[5],
        entry(
            &["minimax-coding-plan", "minimax-cn-coding-plan"],
            "MiniMax-M3",
            None
        ),
        "{agent}"
    );
    assert!(has(&chain[6], "opencode-go"), "{agent}");
    assert_eq!(chain[6].model, "minimax-m2.7", "{agent}");
    assert!(has(&chain[7], "anthropic"), "{agent}");
    assert_eq!(chain[7].model, "claude-haiku-4-5", "{agent}");
    assert!(has(&chain[8], "openai"), "{agent}");
    assert_eq!(chain[8].model, "gpt-5.4-nano", "{agent}");
}

#[test]
fn librarian_keeps_fast_openai_primary_before_qwen_minimax_haiku_and_nano_fallbacks() {
    assert_fast_openai_primary_chain("librarian");
}

#[test]
fn explore_keeps_fast_openai_primary_before_qwen_minimax_haiku_and_nano_fallbacks() {
    assert_fast_openai_primary_chain("explore");
}

#[test]
fn multimodal_looker_keeps_vision_capable_fallback_order() {
    let chain = chain("multimodal-looker");

    assert_eq!(chain.len(), 4);
    assert_eq!(
        chain[0],
        entry(
            &["openai", "opencode", "vercel"],
            "gpt-5.6-sol",
            Some("low")
        )
    );
    assert_eq!(chain[1], entry(&["opencode-go", "vercel"], "kimi-k3", None));
    assert_eq!(chain[2].model, "glm-4.6v");
    assert_eq!(
        chain[3],
        entry(
            &["openai", "github-copilot", "opencode", "vercel"],
            "gpt-5-nano",
            None
        )
    );
}

#[test]
fn prometheus_uses_fable_5_xhigh_before_kimi_k3_max() {
    let chain = chain("prometheus");

    assert_eq!(chain.len(), 2);
    assert_eq!(
        chain[0],
        entry(
            &["anthropic", "github-copilot", "opencode", "vercel"],
            "claude-fable-5",
            Some("xhigh")
        )
    );
    assert_eq!(
        chain[1],
        entry(
            &[
                "opencode-go",
                "kimi-for-coding",
                "moonshotai",
                "opencode",
                "vercel"
            ],
            "kimi-k3",
            Some("max")
        )
    );
}

#[test]
fn metis_uses_opus_5_high_before_kimi_k3_low() {
    let chain = chain("metis");

    assert_eq!(chain.len(), 2);
    assert_eq!(
        chain[0],
        entry(
            &["anthropic", "github-copilot", "opencode", "vercel"],
            "claude-opus-5",
            Some("high")
        )
    );
    assert_eq!(
        chain[1],
        entry(
            &[
                "opencode-go",
                "kimi-for-coding",
                "moonshotai",
                "opencode",
                "vercel"
            ],
            "kimi-k3",
            Some("low")
        )
    );
}

#[test]
fn momus_keeps_native_gpt_5_6_terra_high_before_gpt_5_6_sol_xhigh() {
    let chain = chain("momus");

    assert!(chain.len() > 1);
    assert_eq!(
        chain[0],
        entry(&["openai", "vercel"], "gpt-5.6-terra", Some("high"))
    );
    assert_eq!(
        chain[1],
        entry(&["github-copilot"], "gpt-5.6-terra", Some("high"))
    );
    assert_eq!(
        chain[2],
        entry(
            &["openai", "opencode", "vercel"],
            "gpt-5.6-sol",
            Some("xhigh")
        )
    );
    assert_eq!(
        chain[3],
        entry(&["github-copilot"], "gpt-5.6-sol", Some("high"))
    );
    assert_eq!(
        chain[4],
        entry(
            &["anthropic", "github-copilot", "opencode", "vercel"],
            "claude-opus-5",
            Some("max")
        )
    );
}

#[test]
fn atlas_keeps_sonnet_kimi_sol_and_minimax_fallback_order() {
    let chain = chain("atlas");

    assert_eq!(chain.len(), 6);
    assert_eq!(chain[0].model, "claude-sonnet-5");
    assert_eq!(chain[0].providers[0], "anthropic");
    assert_eq!(chain[1].model, "kimi-k3");
    assert_eq!(chain[1].providers[0], "opencode-go");
    assert_eq!(
        chain[2],
        entry(
            &["openai", "github-copilot", "opencode", "vercel"],
            "gpt-5.6-sol",
            Some("medium")
        )
    );
    assert_eq!(chain[3].model, "minimax-m3");
    assert_eq!(chain[3].providers[0], "opencode-go");
    assert_eq!(
        chain[4],
        entry(
            &["minimax-coding-plan", "minimax-cn-coding-plan"],
            "MiniMax-M3",
            None
        )
    );
    assert_eq!(chain[5].model, "minimax-m2.7");
    assert_eq!(chain[5].providers[0], "opencode-go");
}

#[test]
fn sisyphus_junior_keeps_sonnet_kimi_minimax_and_big_pickle_fallbacks() {
    let model_ids: Vec<&str> = chain("sisyphus-junior")
        .iter()
        .map(|entry| entry.model.as_str())
        .collect();

    assert_eq!(
        model_ids,
        vec![
            "claude-sonnet-5",
            "kimi-k3",
            "gpt-5.6-sol",
            "minimax-m3",
            "MiniMax-M3",
            "minimax-m2.7",
            "big-pickle"
        ]
    );
    assert!(!model_ids.contains(&"gpt-5.5"));
}

#[test]
fn hephaestus_supports_openai_github_copilot_opencode_and_vercel_providers() {
    let hephaestus = &AGENT_MODEL_REQUIREMENTS["hephaestus"];

    assert_eq!(
        hephaestus.requires_provider,
        Some(strings(&["openai", "github-copilot", "opencode", "vercel"]))
    );
    assert!(
        !hephaestus.fallback_chain[0]
            .providers
            .iter()
            .any(|p| p == "venice")
    );
    assert_eq!(hephaestus.requires_model, None);
    assert_eq!(hephaestus.requires_any_model, Some(true));
}

#[test]
fn hephaestus_has_one_merged_gpt_5_6_sol_medium_rung() {
    let chain = chain("hephaestus");

    assert_eq!(chain.len(), 1);
    assert_eq!(
        chain[0],
        entry(
            &["openai", "github-copilot", "vercel", "opencode"],
            "gpt-5.6-sol",
            Some("medium")
        )
    );
}
