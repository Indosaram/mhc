use model_core::AGENT_MODEL_REQUIREMENTS;
use model_core::FallbackEntry;
use pretty_assertions::assert_eq;

use crate::support::entry;
use crate::support::strings;

fn chain(agent: &str) -> &'static [FallbackEntry] {
    &AGENT_MODEL_REQUIREMENTS[agent].fallback_chain
}

#[test]
fn oracle_has_gpt_5_6_sol_xhigh_as_primary() {
    let oracle = chain("oracle");

    assert!(!oracle.is_empty());
    assert_eq!(
        oracle[0],
        entry(
            &["openai", "chatgpt-subscription", "opencode"],
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
fn sisyphus_keeps_opus_primary_before_kimi_k3_gpt_5_6_sol_glm_5_2_and_big_pickle_fallbacks() {
    let sisyphus = &AGENT_MODEL_REQUIREMENTS["sisyphus"];
    let chain = &sisyphus.fallback_chain;

    assert_eq!(chain.len(), 5);
    assert_eq!(sisyphus.requires_any_model, Some(true));
    assert_eq!(
        chain[0],
        entry(
            &["anthropic", "github-copilot", "opencode"],
            "claude-opus-5-5",
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
            &[
                "openai",
                "chatgpt-subscription",
                "github-copilot",
                "opencode"
            ],
            "gpt-5.6-sol",
            Some("medium")
        )
    );
    assert_eq!(chain[3].providers[0], "zai-coding-plan");
    assert_eq!(chain[3].model, "glm-5.2");
    assert_eq!(chain[4].providers[0], "opencode");
    assert_eq!(chain[4].model, "big-pickle");
}

fn assert_fast_agent_chain(agent: &str) {
    let requirement = &AGENT_MODEL_REQUIREMENTS[agent];

    assert_eq!(
        requirement.fallback_chain,
        vec![
            entry(
                &["kimi-for-coding"],
                "kimi-for-coding-highspeed",
                Some("off")
            ),
            entry(
                &["openai", "chatgpt-subscription"],
                "gpt-6-luna-fast",
                Some("low")
            ),
            entry(&["anthropic", "github-copilot"], "claude-haiku-5-5", Some("medium")),
            entry(&["deepseek"], "deepseek-flash", Some("max")),
            entry(
                &["opencode-go", "bailian-coding-plan"],
                "qwen3.7-plus",
                None
            ),
            entry(&["opencode-go"], "minimax-m2.7", None),
        ],
        "{agent}"
    );
}

#[test]
fn librarian_runs_no_thinking_kimi_highspeed_luna_fast_deepseek_flash_qwen_m2_7_then_haiku() {
    assert_fast_agent_chain("librarian");
}

#[test]
fn explore_runs_no_thinking_kimi_highspeed_luna_fast_deepseek_flash_qwen_m2_7_then_haiku() {
    assert_fast_agent_chain("explore");
}

#[test]
fn multimodal_looker_keeps_vision_capable_fallback_order() {
    let chain = chain("multimodal-looker");

    assert_eq!(chain.len(), 4);
    assert_eq!(
        chain[0],
        entry(
            &["openai", "chatgpt-subscription", "opencode"],
            "gpt-5.6-sol",
            Some("low")
        )
    );
    assert_eq!(chain[1], entry(&["opencode-go"], "kimi-k3", None));
    assert_eq!(chain[2].model, "glm-4.6v");
    assert_eq!(
        chain[3],
        entry(
            &[
                "openai",
                "chatgpt-subscription",
                "github-copilot",
                "opencode"
            ],
            "gpt-5-nano",
            None
        )
    );
}

#[test]
fn prometheus_uses_fable_5_1_xhigh_then_opus_5_5_max_before_kimi_k3_max() {
    let chain = chain("prometheus");

    assert_eq!(chain.len(), 3);
    assert_eq!(
        chain[0],
        entry(
            &["anthropic", "github-copilot", "opencode"],
            "claude-fable-5-1",
            Some("xhigh")
        )
    );
    assert_eq!(
        chain[1],
        entry(
            &["anthropic", "github-copilot", "opencode"],
            "claude-opus-5-5",
            Some("max")
        )
    );
    assert_eq!(
        chain[2],
        entry(
            &[
                "opencode-go",
                "kimi-for-coding",
                "moonshotai",
                "opencode"
            ],
            "kimi-k3",
            Some("max")
        )
    );
}

#[test]
fn metis_uses_fable_5_1_max_then_opus_5_max_then_kimi_k3_max() {
    let chain = chain("metis");

    assert_eq!(chain.len(), 3);
    assert_eq!(
        chain[0],
        entry(
            &["anthropic", "github-copilot", "opencode"],
            "claude-fable-5-1",
            Some("max")
        )
    );
    assert_eq!(
        chain[1],
        entry(
            &["anthropic", "github-copilot", "opencode"],
            "claude-opus-5-5",
            Some("max")
        )
    );
    assert_eq!(
        chain[2],
        entry(
            &[
                "opencode-go",
                "kimi-for-coding",
                "moonshotai",
                "opencode"
            ],
            "kimi-k3",
            Some("max")
        )
    );
}

#[test]
fn momus_leads_with_native_gpt_6_astra_xhigh_before_copilot_and_opencode_astra_rungs() {
    let chain = chain("momus");

    assert!(chain.len() > 1);
    assert_eq!(
        chain[0],
        entry(
            &["openai", "chatgpt-subscription"],
            "gpt-6-astra",
            Some("xhigh")
        )
    );
    assert_eq!(
        chain[1],
        entry(&["github-copilot"], "gpt-6-astra", Some("high"))
    );
    assert_eq!(
        chain[2],
        entry(
            &["openai", "chatgpt-subscription", "opencode"],
            "gpt-6-astra",
            Some("high")
        )
    );
    assert_eq!(
        chain[3],
        entry(
            &["anthropic", "github-copilot", "opencode"],
            "claude-opus-5-5",
            Some("max")
        )
    );
    assert_eq!(
        chain[5],
        entry(
            &["google", "github-copilot", "opencode"],
            "gemini-3.1-pro",
            Some("high")
        )
    );
    assert_eq!(chain[4], entry(&["google", "google-vertex", "opencode"], "gemini-4-argon", Some("high")));
    assert_eq!(chain[6], entry(&["opencode-go"], "glm-5.2", None));
    assert!(!chain.iter().any(|entry| entry.model.starts_with("gpt-5.6-")));
}

#[test]
fn atlas_keeps_sonnet_kimi_gpt_5_6_sol_and_minimax_fallback_order() {
    let chain = chain("atlas");

    assert_eq!(chain.len(), 6);
    assert_eq!(chain[0].model, "claude-sonnet-5");
    assert_eq!(chain[0].providers[0], "anthropic");
    assert_eq!(chain[1].model, "kimi-k3");
    assert_eq!(chain[1].providers[0], "opencode-go");
    assert_eq!(
        chain[2],
        entry(
            &[
                "openai",
                "chatgpt-subscription",
                "github-copilot",
                "opencode"
            ],
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
fn hephaestus_supports_openai_chatgpt_subscription_github_copilot_and_opencode_providers() {
    let hephaestus = &AGENT_MODEL_REQUIREMENTS["hephaestus"];

    assert_eq!(
        hephaestus.requires_provider,
        Some(strings(&[
            "openai",
            "chatgpt-subscription",
            "github-copilot",
            "opencode"
        ]))
    );
    assert!(
        !hephaestus.requires_provider
            .as_ref()
            .unwrap()
            .iter()
            .any(|p| p == "venice")
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
fn hephaestus_leads_with_one_merged_gpt_6_sol_medium_rung_over_its_gpt_5_6_sol_predecessor() {
    let chain = chain("hephaestus");

    assert_eq!(chain.len(), 2);
    assert_eq!(
        chain[0],
        entry(
            &[
                "openai",
                "chatgpt-subscription",
                "github-copilot",
                "opencode"
            ],
            "gpt-6-sol",
            Some("medium")
        )
    );
    assert_eq!(
        chain[1],
        entry(
            &[
                "openai",
                "chatgpt-subscription",
                "github-copilot",
                "opencode"
            ],
            "gpt-5.6-sol",
            Some("medium")
        )
    );
}
