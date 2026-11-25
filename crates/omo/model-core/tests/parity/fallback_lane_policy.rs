//! Built-in lanes exclude gateways and pair OpenAI with its subscription provider.
use model_core::{AGENT_MODEL_REQUIREMENTS, CATEGORY_MODEL_REQUIREMENTS};

#[test]
fn every_builtin_rung_obeys_the_provider_lane_policy() {
    for (name, requirement) in AGENT_MODEL_REQUIREMENTS
        .iter()
        .chain(CATEGORY_MODEL_REQUIREMENTS.iter())
    {
        for rung in &requirement.fallback_chain {
            let lists = |provider: &str| rung.providers.iter().any(|id| id == provider);
            assert!(!lists("vercel"), "{name}: {} lists vercel", rung.model);
            assert!(
                !lists("quotio-openai"),
                "{name}: {} lists quotio-openai",
                rung.model
            );
            assert!(
                !lists("openai") || lists("chatgpt-subscription"),
                "{name}: {} lacks chatgpt-subscription",
                rung.model
            );
        }
    }
}
