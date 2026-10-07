use model_core::DEVIN_SWE2_SERVED_LANES;
use model_core::is_claude_fable_or_mythos_model;
use model_core::is_claude_fable5_model;
use model_core::is_claude_opus5_model;
use model_core::is_claude_opus46_model;
use model_core::is_claude_opus47_model;
use model_core::is_claude_opus47_or_later_model;
use model_core::is_claude_opus48_model;
use model_core::is_gemini_model;
use model_core::is_glm_model;
use model_core::is_gpt_model;
use model_core::is_grok45_model;
use model_core::is_grok46_model;
use model_core::is_kimi_k2_model;
use model_core::is_kimi_k3_model;
use model_core::is_kimi_k27_model;
use model_core::is_mini_max_model;
use model_core::is_swe2_model;
use model_core::is_unserved_devin_swe2_selector;

fn check(detector: fn(&str) -> bool, cases: &[(&str, bool)]) {
    for (model, expected) in cases {
        assert_eq!(detector(model), *expected, "{model}");
    }
}

#[test]
fn gpt_model_ids_detect_gpt_family_only() {
    check(
        is_gpt_model,
        &[
            ("openai/gpt-5.5", true),
            ("github-copilot/gpt-4o", true),
            ("openai/o3-mini", false),
            ("anthropic/claude-opus-4-7", false),
        ],
    );
}

#[test]
fn gemini_model_ids_detect_gemini_family_only() {
    check(
        is_gemini_model,
        &[
            ("google/gemini-3.1-pro", true),
            ("google-vertex/gemini-3-flash", true),
            ("github-copilot/gemini-3.1-pro", true),
            ("openai/gpt-5.5", false),
        ],
    );
}

#[test]
fn kimi_k2_model_ids_detect_kimi_k2_family_only() {
    check(
        is_kimi_k2_model,
        &[
            ("moonshotai/kimi-k2.6", true),
            ("opencode/k2p5", true),
            ("opencode/k2-p6", true),
            ("anthropic/claude-opus-4-7", false),
        ],
    );
}

#[test]
fn kimi_k2_7_model_ids_detect_k2_7_only_not_k2_6() {
    check(
        is_kimi_k27_model,
        &[
            ("opencode-go/kimi-k2.7", true),
            ("moonshotai/kimi-k2-7", true),
            ("kimi-for-coding/k2p7", true),
            ("opencode/k2-p7", true),
            ("opencode-go/kimi-k2.6", false),
            ("kimi-for-coding/k2p6", false),
            ("kimi-for-coding/k2p5", false),
            ("anthropic/claude-opus-4-7", false),
        ],
    );
    assert!(is_kimi_k2_model("opencode-go/kimi-k2.7"));
}

#[test]
fn kimi_k3_model_ids_detect_k3_only_not_k2_x() {
    check(
        is_kimi_k3_model,
        &[
            ("opencode-go/kimi-k3", true),
            ("moonshotai/kimi-k3-202607", true),
            ("kimi-for-coding/k3p1", true),
            ("opencode/k3", true),
            ("opencode-go/kimi-k2.7", false),
            ("kimi-for-coding/k2p7", false),
            ("kimi-for-coding/k2p5", false),
            ("anthropic/claude-opus-4-7", false),
        ],
    );
}

#[test]
fn devin_selectors_only_an_explicit_devin_swe2_id_outside_the_served_lanes_is_unserved() {
    assert_eq!(DEVIN_SWE2_SERVED_LANES, ["swe-2-medium", "swe-2-high", "swe-2-max"]);
    check(
        is_unserved_devin_swe2_selector,
        &[
            ("devin/swe-2-medium", false),
            ("devin/swe-2-high", false),
            ("devin/swe-2-max", false),
            ("Devin/SWE-2-High", false),
            ("devin/swe-2-high:max", false),
            ("devin/swe-2-medium (high)", false),
            ("devin/swe-2", true),
            ("devin/swe-2-low", true),
            ("devin/swe-2-high-lite", true),
            ("devin/swe-2.0", true),
            ("devin/swe-2-low:high", true),
            ("swe-2-low", false),
            ("gateway/swe-2-low", false),
            ("devin/swe-1-6", false),
            ("devin/swe-20", false),
            ("devin/adaptive", false),
        ],
    );
}

#[test]
fn devin_swe2_model_ids_detect_swe2_effort_lanes_only() {
    check(
        is_swe2_model,
        &[
            ("devin/swe-2-low", true),
            ("devin/swe-2-high", true),
            ("devin/swe-2-max", true),
            ("swe-2", true),
            ("devin/swe-1-7", false),
            ("devin/swe-20", false),
        ],
    );
}

#[test]
fn glm_model_ids_detect_glm_family_only() {
    check(
        is_glm_model,
        &[
            ("z-ai/glm-5.1", true),
            ("opencode/glm-4.6v", true),
            ("google/gemini-3.1-pro", false),
        ],
    );
}

#[test]
fn claude_opus_4_6_model_ids_detect_opus_4_6_only() {
    check(
        is_claude_opus46_model,
        &[
            ("anthropic/claude-opus-4-6", true),
            ("anthropic/claude-opus-4.6", true),
            ("claude-opus-4-6", true),
            ("anthropic/claude-opus-4-7", false),
            ("anthropic/claude-sonnet-4-6", false),
        ],
    );
}

#[test]
fn claude_opus_4_7_model_ids_detect_opus_4_7_only() {
    check(
        is_claude_opus47_model,
        &[
            ("anthropic/claude-opus-4-7", true),
            ("anthropic/claude-opus-4.7", true),
            ("anthropic/claude-sonnet-4-6", false),
        ],
    );
}

#[test]
fn claude_opus_4_8_model_ids_detect_opus_4_8_only() {
    check(
        is_claude_opus48_model,
        &[
            ("anthropic/claude-opus-4-8", true),
            ("anthropic/claude-opus-4.8", true),
            ("anthropic/claude-opus-4-7", false),
            ("anthropic/claude-fable-5", false),
        ],
    );
}

#[test]
fn claude_opus_5_model_ids_detect_opus_5_only() {
    check(
        is_claude_opus5_model,
        &[
            ("anthropic/claude-opus-5", true),
            ("anthropic/claude-opus-5-0", true),
            ("anthropic/claude-opus-5.0", true),
            ("anthropic/claude-opus-5[1m]", true),
            ("claude-opus-5", true),
            ("anthropic/claude-opus-4-8", false),
            ("anthropic/claude-fable-5", false),
            ("anthropic/claude-sonnet-4-6", false),
        ],
    );
}

#[test]
fn claude_fable_5_model_ids_detect_fable_5_only() {
    check(
        is_claude_fable5_model,
        &[
            ("anthropic/claude-fable-5", true),
            ("anthropic/claude-fable-5[1m]", true),
            ("claude-fable-5", true),
            ("anthropic/claude-opus-4-8", false),
            ("anthropic/claude-sonnet-4-6", false),
        ],
    );
}

#[test]
fn claude_opus_4_7_or_later_model_ids_detect_4_7_and_later_only() {
    check(
        is_claude_opus47_or_later_model,
        &[
            ("anthropic/claude-opus-4-7", true),
            ("anthropic/claude-opus-4-8", true),
            ("anthropic/claude-opus-4.8", true),
            ("anthropic/claude-opus-5-0", true),
            ("anthropic/claude-opus-5", true),
            ("anthropic/claude-opus-4", false),
            ("claude-opus-4-7", true),
            ("anthropic/claude-fable-5", true),
            ("anthropic/claude-fable-5[1m]", true),
            ("anthropic/claude-opus-4-6", false),
            ("anthropic/claude-sonnet-4-6", false),
            ("openai/gpt-5.5", false),
        ],
    );
}

#[test]
fn claude_fable_or_mythos_model_ids_detect_fable_and_mythos_families() {
    check(
        is_claude_fable_or_mythos_model,
        &[
            ("anthropic/claude-fable-5", true),
            ("claude-fable-5", true),
            ("anthropic.claude-fable-5", true),
            ("anthropic/claude-mythos-5", true),
            ("anthropic/claude-mythos-preview", true),
            ("anthropic/claude-opus-4-8", false),
            ("anthropic/claude-sonnet-4-6", false),
            ("openai/gpt-5.5", false),
        ],
    );
}

#[test]
fn grok_4_5_model_ids_detect_grok_4_5_only() {
    check(
        is_grok45_model,
        &[
            ("xai/grok-4.5", true),
            ("x-ai/grok-4.5", true),
            ("grok-4-5", true),
            ("openrouter/grok-4.5-fast", true),
            ("xai/grok-4.6", false),
            ("xai/grok-4", false),
            ("x-ai/grok-4.20", false),
            ("xai/grok-4-1-fast-reasoning", false),
            ("x-ai/grok-code-fast-1", false),
        ],
    );
}

#[test]
fn grok_4_6_model_ids_detect_grok_4_6_only() {
    check(
        is_grok46_model,
        &[
            ("xai/grok-4.6", true),
            ("x-ai/grok-4.6", true),
            ("grok-4-6", true),
            ("openrouter/grok-4.6-fast", true),
            ("xai/grok-4.5", false),
            ("xai/grok-4", false),
            ("x-ai/grok-4.20", false),
            ("xai/grok-4-1-fast-reasoning", false),
            ("x-ai/grok-code-fast-1", false),
        ],
    );
}

#[test]
fn minimax_model_ids_detect_minimax_family_only() {
    check(
        is_mini_max_model,
        &[
            ("opencode/minimax-m2.7", true),
            ("minimax-m2.7-highspeed", true),
            ("moonshotai/kimi-k2.6", false),
        ],
    );
}
