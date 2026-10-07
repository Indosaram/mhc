mod support;

use indexmap::IndexMap;
use maho_omo_model_profile::builtin_profiles::{
    BuiltinModelProfile, DEFAULT_MODEL_PROFILE_ID, ModelProfileFamily, ModelProfileTier,
    builtin_model_profiles,
};

const LANE_IDS: [&str; 4] = ["daily-normal", "daily-heavy", "geeky-normal", "geeky-heavy"];
const EXPECTED_IDS: [&str; 5] = ["recommended", "daily-normal", "daily-heavy", "geeky-normal", "geeky-heavy"];
const BANNED_VENDOR_TOKENS: [&str; 2] = ["minimax", "gemini"];

fn profiles() -> IndexMap<&'static str, BuiltinModelProfile> {
    builtin_model_profiles()
}

fn chain_of(profile: &str) -> Vec<String> {
    profiles()[profile]
        .models
        .iter()
        .map(|rung| format!("{} {}", rung.model, rung.variant.unwrap_or("default")))
        .collect()
}

fn rungs() -> Vec<(&'static str, Vec<&'static str>, &'static str)> {
    profiles()
        .into_iter()
        .flat_map(|(profile, definition)| {
            definition
                .models
                .iter()
                .map(move |rung| (profile, rung.providers.to_vec(), rung.model))
        })
        .collect()
}

#[test]
fn ships_the_recommended_default_then_the_four_lane_profiles_in_picker_order() {
    let ids: Vec<&str> = profiles().keys().copied().collect();
    assert_eq!(ids, EXPECTED_IDS);
}

#[test]
fn labels_each_profile_by_lane_and_gives_every_profile_a_distinct_family_tier_pair() {
    let profiles = profiles();
    assert_eq!(profiles["daily-normal"].display_name, "Daily · Normal");
    assert_eq!(profiles["daily-heavy"].display_name, "Daily · Heavy");
    assert_eq!(profiles["geeky-normal"].display_name, "Geeky · Normal");
    assert_eq!(profiles["geeky-heavy"].display_name, "Geeky · Heavy");
    let pairs: Vec<String> = LANE_IDS
        .iter()
        .map(|id| format!("{}:{}", profiles[*id].family.unwrap().as_str(), profiles[*id].tier.unwrap().as_str()))
        .collect();
    assert_eq!(pairs, ["daily:normal", "daily:heavy", "geeky:normal", "geeky:heavy"]);
    for id in EXPECTED_IDS {
        assert!(!profiles[id].description.is_empty(), "{id} carries a description");
    }
}

#[test]
fn uses_recommended_which_is_not_a_lane_as_the_unset_config_default_id() {
    assert_eq!(DEFAULT_MODEL_PROFILE_ID, "recommended");
    let recommended = &profiles()[DEFAULT_MODEL_PROFILE_ID];
    assert_eq!(recommended.display_name, "Recommended");
    assert_eq!(recommended.family, None);
    assert_eq!(recommended.tier, None);
}

// Mirrors senpi's RECOMMENDED_DEFAULT_MODELS, so the TUI and the desktop start from one order.
#[test]
fn orders_recommended_opus_medium_fable_xhigh_kimi_max_astra_xhigh_61_sol_medium_6_sol_medium_glm_max() {
    assert_eq!(
        chain_of("recommended"),
        [" medium", " xhigh", "kimi-k3 max", "gpt-6-astra xhigh", "gpt-6.1-sol medium", "gpt-6-sol medium", "glm-5.3 max"]
    );
}

#[test]
fn serves_recommendeds_gpt_61_sol_rung_on_the_oi_lanes_and_its_gpt_6_sol_rung_on_every_gpt_lane() {
    let sol: Vec<(Vec<&str>, &str, Option<&str>)> = profiles()["recommended"]
        .models
        .iter()
        .filter(|rung| rung.model.contains("-sol"))
        .map(|rung| (rung.providers.to_vec(), rung.model, rung.variant))
        .collect();
    assert_eq!(
        sol,
        [
            (vec!["chatgpt-subscription", "openai"], "gpt-6.1-sol", Some("medium")),
            (vec!["chatgpt-subscription", "openai", "github-copilot", "opencode"], "gpt-6-sol", Some("medium")),
        ]
    );
}

#[test]
fn never_lists_a_gateway_aggregator_on_a_recommended_rung() {
    let gateways = ["opengateway", "openrouter", "vercel-ai-gateway", "cloudflare-ai-gateway"];
    let offenders: Vec<&str> = profiles()["recommended"]
        .models
        .iter()
        .flat_map(|rung| rung.providers.iter().copied())
        .filter(|provider| gateways.contains(provider))
        .collect();
    assert_eq!(offenders, Vec::<&str>::new());
}

#[test]
fn gives_every_rung_at_least_one_provider_and_a_model_id() {
    let rungs = rungs();
    assert!(!rungs.is_empty());
    let invalid: Vec<String> = rungs
        .iter()
        .filter(|(_, providers, model)| model.trim().is_empty() || providers.is_empty())
        .map(|(profile, providers, model)| format!("{profile}: {}/{}", providers.join("|"), model))
        .collect();
    assert_eq!(invalid, Vec::<String>::new());
}

#[test]
fn routes_every_rung_through_a_provider_the_product_already_knows() {
    let unknown: Vec<String> = rungs()
        .iter()
        .flat_map(|(profile, providers, _)| {
            providers
                .iter()
                .filter(|provider| !support::is_known_provider(provider))
                .map(move |provider| format!("{profile}: {provider}"))
        })
        .collect();
    assert_eq!(unknown, Vec::<String>::new());
}

#[test]
fn names_no_banned_vendor() {
    let offenders: Vec<String> = rungs()
        .iter()
        .flat_map(|(_, providers, model)| providers.iter().copied().chain(std::iter::once(*model)))
        .filter(|token| BANNED_VENDOR_TOKENS.iter().any(|banned| token.to_lowercase().contains(banned)))
        .map(str::to_owned)
        .collect();
    assert_eq!(offenders, Vec::<String>::new());
}

#[test]
fn ranks_the_chatgpt_subscription_ahead_of_the_openai_api_lane_on_every_gpt_rung() {
    let gpt: Vec<(&str, Vec<&str>, &str)> = rungs()
        .into_iter()
        .filter(|(_, _, model)| model.starts_with("gpt-"))
        .collect();
    assert!(!gpt.is_empty());
    let misordered: Vec<String> = gpt
        .iter()
        .filter(|(_, providers, _)| providers.first() != Some(&"chatgpt-subscription") || providers.get(1) != Some(&"openai"))
        .map(|(profile, providers, model)| format!("{profile}: {}/{model}", providers.join("|")))
        .collect();
    assert_eq!(misordered, Vec::<String>::new());
}

#[test]
fn heads_every_glm_rung_with_engine_zai_then_zai_coding_cn() {
    let glm: Vec<(&str, Vec<&str>, &str)> = rungs()
        .into_iter()
        .filter(|(_, _, model)| model.starts_with("glm-"))
        .collect();
    assert!(!glm.is_empty());
    let misordered: Vec<String> = glm
        .iter()
        .filter(|(_, providers, _)| providers.first() != Some(&"zai") || providers.get(1) != Some(&"zai-coding-cn"))
        .map(|(profile, providers, model)| format!("{profile}: {}/{model}", providers.join("|")))
        .collect();
    assert_eq!(misordered, Vec::<String>::new());
}

#[test]
fn heads_every_claude_rung_with_the_anthropic_subscription_lane() {
    let claude: Vec<(&str, Vec<&str>, &str)> = rungs()
        .into_iter()
        .filter(|(_, _, model)| model.starts_with("claude-") || model.is_empty())
        .collect();
    assert!(!claude.is_empty());
    let misordered: Vec<String> = claude
        .iter()
        .filter(|(_, providers, _)| providers.first() != Some(&"anthropic-subscription"))
        .map(|(profile, _, model)| format!("{profile}: {model}"))
        .collect();
    assert_eq!(misordered, Vec::<String>::new());
}

#[test]
fn orders_daily_normal_opus_medium_then_kimi_max_then_glm_max() {
    assert_eq!(chain_of("daily-normal"), [" medium", "kimi-k3 max", "glm-5.3 max"]);
}

#[test]
fn runs_daily_heavy_as_fable_xhigh_only() {
    let models: Vec<(Vec<&str>, &str, Option<&str>)> = profiles()["daily-heavy"]
        .models
        .iter()
        .map(|rung| (rung.providers.to_vec(), rung.model, rung.variant))
        .collect();
    assert_eq!(
        models,
        [(
            vec!["anthropic-subscription", "anthropic", "anthropic-api", "github-copilot", "opencode"],
            "",
            Some("xhigh")
        )]
    );
}

#[test]
fn runs_geeky_normal_as_gpt_61_sol_fast_then_gpt_61_sol_medium_on_the_oi_lanes_then_gpt_56_sol_medium_on_every_gpt_lane() {
    let models: Vec<(Vec<&str>, &str, Option<&str>)> = profiles()["geeky-normal"]
        .models
        .iter()
        .map(|rung| (rung.providers.to_vec(), rung.model, rung.variant))
        .collect();
    assert_eq!(
        models,
        [
            (vec!["chatgpt-subscription", "openai"], "gpt-6.1-sol-fast", Some("medium")),
            (vec!["chatgpt-subscription", "openai"], "gpt-6.1-sol", Some("medium")),
            (vec!["chatgpt-subscription", "openai", "github-copilot", "opencode"], "gpt-5.6-sol", Some("medium")),
        ]
    );
}

#[test]
fn runs_geeky_heavy_as_astra_high_with_the_same_provider_ranking_as_deep_high() {
    let models: Vec<(Vec<&str>, &str, Option<&str>)> = profiles()["geeky-heavy"]
        .models
        .iter()
        .map(|rung| (rung.providers.to_vec(), rung.model, rung.variant))
        .collect();
    assert_eq!(
        models,
        [(vec!["chatgpt-subscription", "openai", "github-copilot", "opencode"], "gpt-6-astra", Some("high"))]
    );
}

#[test]
fn keeps_recommended_off_both_picker_axes_and_every_lane_on_both() {
    let profiles = profiles();
    assert_eq!(profiles["recommended"].family, None);
    assert_eq!(profiles["recommended"].tier, None);
    assert_eq!(profiles["daily-normal"].family, Some(ModelProfileFamily::Daily));
    assert_eq!(profiles["daily-normal"].tier, Some(ModelProfileTier::Normal));
    assert_eq!(profiles["geeky-heavy"].family, Some(ModelProfileFamily::Geeky));
    assert_eq!(profiles["geeky-heavy"].tier, Some(ModelProfileTier::Heavy));
}
