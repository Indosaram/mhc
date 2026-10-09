use model_core::AGENT_MODEL_REQUIREMENTS;
use model_core::CATEGORY_MODEL_REQUIREMENTS;
use pretty_assertions::assert_eq;

use crate::support::entry;

#[test]
fn quick_places_non_reasoning_deepseek_v4_flash_immediately_after_luna() {
    let quick = &CATEGORY_MODEL_REQUIREMENTS["quick"].fallback_chain;

    assert_eq!(
        quick[0..2].to_vec(),
        vec![
            entry(&["openai", "chatgpt-subscription"], "gpt-6-luna-fast", Some("low")),
            entry(&["deepseek"], "deepseek-flash", Some("off")),
        ]
    );
}

#[test]
fn explore_and_librarian_place_max_reasoning_deepseek_v4_flash_immediately_after_luna() {
    for agent_name in ["explore", "librarian"] {
        let chain = &AGENT_MODEL_REQUIREMENTS[agent_name].fallback_chain;

        assert_eq!(
            chain[0..3].to_vec(),
            vec![
                entry(&["kimi-for-coding"], "kimi-for-coding-highspeed", Some("off")),
                entry(&["openai", "chatgpt-subscription"], "gpt-6-luna-fast", Some("low")),
                entry(&["deepseek"], "deepseek-flash", Some("max")),
            ],
            "{agent_name}"
        );
    }
}
