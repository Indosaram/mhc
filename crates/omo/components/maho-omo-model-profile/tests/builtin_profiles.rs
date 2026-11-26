use crate::support;

use indexmap::IndexMap;
use maho_omo_model_profile::builtin_profiles::{
    BuiltinModelProfile, DEFAULT_MODEL_PROFILE_ID, DEFAULT_MODEL_PROFILE_ORDER,
    RETIRED_MODEL_PROFILE_IDS, builtin_model_profiles,
};

const LANE_IDS: [&str; 4] = ["daily-normal", "daily-heavy", "geeky-normal", "geeky-heavy"];
const EXPECTED_IDS: [&str; 4] = LANE_IDS;
const BANNED_VENDOR_TOKENS: [&str; 2] = ["minimax", "gemini"];

fn profiles() -> IndexMap<&'static str, BuiltinModelProfile> {
    builtin_model_profiles()
}

type ExpectedRung = (&'static [&'static str], &'static str, Option<&'static str>);

fn chain_of(profile: &str) -> Vec<ExpectedRung> {
    profiles()[profile]
        .models
        .iter()
        .map(|rung| (rung.providers, rung.model, rung.variant))
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
fn ships_exactly_the_four_lane_profiles_in_picker_order() {
    let ids: Vec<&str> = profiles().keys().copied().collect();
    assert_eq!(ids, EXPECTED_IDS);
}

#[test]
fn labels_each_profile_by_lane_and_gives_every_profile_a_distinct_family_tier_pair() {
    let profiles = profiles();
    assert_eq!(profiles["daily-normal"].display_name, "Daily · Normal");
    assert_eq!(profiles["daily-heavy"].display_name, "Daily · Heavy");
    assert_eq!(profiles["geeky-normal"].display_name, "Obsessive · Normal");
    assert_eq!(profiles["geeky-heavy"].display_name, "Obsessive · Heavy");
    let pairs: Vec<String> = LANE_IDS
        .iter()
        .map(|id| {
            format!(
                "{}:{}",
                profiles[*id].family.as_str(),
                profiles[*id].tier.as_str()
            )
        })
        .collect();
    assert_eq!(
        pairs,
        ["daily:normal", "daily:heavy", "geeky:normal", "geeky:heavy"]
    );
    for id in EXPECTED_IDS {
        assert!(
            !profiles[id].description.is_empty(),
            "{id} carries a description"
        );
    }
}

#[test]
fn carries_the_bold_caution_only_on_the_obsessive_lanes() {
    let profiles = profiles();
    let caution = "NOT RECOMMENDED FOR WRITING OR EVERYDAY WORK.";
    assert_eq!(profiles["geeky-normal"].caution, Some(caution));
    assert_eq!(profiles["geeky-heavy"].caution, Some(caution));
    assert_eq!(profiles["daily-normal"].caution, None);
    assert_eq!(profiles["daily-heavy"].caution, None);
}

#[test]
fn starts_unset_on_daily_normal_and_retires_recommended() {
    assert_eq!(DEFAULT_MODEL_PROFILE_ID, "daily-normal");
    assert_eq!(DEFAULT_MODEL_PROFILE_ORDER, LANE_IDS);
    assert_eq!(RETIRED_MODEL_PROFILE_IDS, &["recommended"]);
    assert!(!profiles().contains_key("recommended"));
}

#[test]
fn never_lists_a_gateway_aggregator_on_a_builtin_rung() {
    let gateways = [
        "opengateway",
        "openrouter",
        "vercel-ai-gateway",
        "cloudflare-ai-gateway",
    ];
    for (_, providers, _) in rungs() {
        assert!(
            providers
                .iter()
                .all(|provider| !gateways.contains(provider))
        );
    }
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
        .filter(|token| {
            BANNED_VENDOR_TOKENS
                .iter()
                .any(|banned| token.to_lowercase().contains(banned))
        })
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
        .filter(|(_, providers, _)| {
            providers.first() != Some(&"chatgpt-subscription")
                || providers.get(1) != Some(&"openai")
        })
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

const CLAUDE_PROVIDERS: &[&str] = &[
    "anthropic-subscription",
    "anthropic",
    "anthropic-api",
    "github-copilot",
    "opencode",
];
const KIMI_PROVIDERS: &[&str] = &[
    "kimi-coding",
    "kimi-for-coding",
    "moonshotai",
    "opencode-go",
];
const GPT_PROVIDERS: &[&str] = &[
    "chatgpt-subscription",
    "openai",
    "github-copilot",
    "opencode",
];
const GPT_6_2_ASTRA_PROVIDERS: &[&str] = &["chatgpt-subscription", "openai", "opencode"];
const GPT_SOL_PROVIDERS: &[&str] = &["chatgpt-subscription", "openai"];

#[test]
fn daily_normal_orders_models_providers_and_effort() {
    assert_eq!(
        chain_of("daily-normal"),
        [
            (CLAUDE_PROVIDERS, "claude-opus-5-5", Some("medium")),
            (CLAUDE_PROVIDERS, "claude-fable-5-2", Some("medium")),
            (KIMI_PROVIDERS, "kimi-k3.1-pro", Some("max")),
        ]
    );
}

#[test]
fn daily_heavy_orders_models_providers_and_effort() {
    assert_eq!(
        chain_of("daily-heavy"),
        [
            (CLAUDE_PROVIDERS, "claude-fable-5-2", Some("high")),
            (CLAUDE_PROVIDERS, "claude-fable-5-1", Some("xhigh")),
        ]
    );
}

#[test]
fn geeky_normal_orders_models_providers_and_effort() {
    assert_eq!(
        chain_of("geeky-normal"),
        [
            (GPT_6_2_ASTRA_PROVIDERS, "gpt-6.2-astra", Some("medium")),
            (GPT_PROVIDERS, "gpt-6-astra", Some("medium")),
            (GPT_SOL_PROVIDERS, "gpt-6.2-sol-fast", Some("medium")),
            (GPT_SOL_PROVIDERS, "gpt-6.2-sol", Some("medium")),
            (GPT_SOL_PROVIDERS, "gpt-6.1-sol-fast", Some("medium")),
            (GPT_SOL_PROVIDERS, "gpt-6.1-sol", Some("medium")),
            (GPT_PROVIDERS, "gpt-5.6-sol", Some("medium")),
        ]
    );
}

#[test]
fn geeky_heavy_orders_models_providers_and_effort() {
    assert_eq!(
        chain_of("geeky-heavy"),
        [
            (GPT_6_2_ASTRA_PROVIDERS, "gpt-6.2-astra", Some("high")),
            (GPT_PROVIDERS, "gpt-6-astra", Some("high")),
        ]
    );
}
