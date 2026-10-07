mod support;

use maho_ext_api::JsonValue;
use maho_omo_model_profile::resolve::{
    ModelProfileResolution, ModelProfileSource, ModelProfileSummary, ResolveModelProfileInput,
    resolve_model_profile,
};
use serde_json::json;

const OPUS: &str = "anthropic/";
const OPUS_SUBSCRIPTION: &str = "anthropic-subscription/";
const OPUS_ZEN: &str = "opencode/";
const OPUS_API: &str = "anthropic-api/";
const FABLE: &str = "anthropic-subscription/";
const FABLE_ZEN: &str = "opencode/";
const KIMI: &str = "moonshotai/kimi-k3";
const FLASH: &str = "deepseek/deepseek-flash";
const LUNA: &str = "openai/gpt-5.6-luna-fast";
const SOL_FAST: &str = "chatgpt-subscription/gpt-6-sol-fast";
const SOL_COPILOT: &str = "github-copilot/gpt-6-sol";
const SOL_56: &str = "chatgpt-subscription/gpt-5.6-sol";
const SOL_56_COPILOT: &str = "github-copilot/gpt-5.6-sol";
const SOL_61: &str = "chatgpt-subscription/gpt-6.1-sol";
const ASTRA: &str = "chatgpt-subscription/gpt-6-astra";

fn resolve(active: &str, available: &[&str], profiles: Option<JsonValue>) -> ModelProfileResolution {
    let owned: Vec<String> = available.iter().map(|selector| (*selector).to_owned()).collect();
    resolve_model_profile(&ResolveModelProfileInput {
        profiles: profiles.as_ref(),
        active,
        available_models: &owned,
    })
}

struct Resolved<'a> {
    provider: &'a str,
    model_id: &'a str,
    reasoning: Option<&'a str>,
    skipped: &'a [String],
}

fn resolved(result: &ModelProfileResolution) -> Resolved<'_> {
    match result {
        ModelProfileResolution::Resolved { provider, model_id, reasoning, skipped, .. } => Resolved {
            provider,
            model_id,
            reasoning: reasoning.as_deref(),
            skipped,
        },
        other => panic!("expected a resolved profile, got {other:?}"),
    }
}

fn profile_of(result: &ModelProfileResolution) -> &ModelProfileSummary {
    match result {
        ModelProfileResolution::Resolved { profile, .. }
        | ModelProfileResolution::Unavailable { profile, .. }
        | ModelProfileResolution::Empty { profile } => profile,
        other => panic!("expected a profile-bearing resolution, got {other:?}"),
    }
}

fn chain_of(result: &ModelProfileResolution) -> &[String] {
    match result {
        ModelProfileResolution::Unavailable { chain, .. } => chain,
        other => panic!("expected an unavailable profile, got {other:?}"),
    }
}

fn expected_resolved(
    profile: &str,
    display_name: &str,
    source: ModelProfileSource,
    provider: &str,
    model_id: &str,
    reasoning: Option<&str>,
    skipped: &[&str],
) -> ModelProfileResolution {
    ModelProfileResolution::Resolved {
        profile: ModelProfileSummary {
            id: profile.to_owned(),
            display_name: display_name.to_owned(),
            source,
            family: None,
            tier: None,
        },
        provider: provider.to_owned(),
        model_id: model_id.to_owned(),
        reasoning: reasoning.map(str::to_owned),
        skipped: skipped.iter().map(|entry| (*entry).to_owned()).collect(),
    }
}

#[test]
fn picks_the_first_rung_the_registry_can_serve_and_names_the_skipped_ones() {
    let result = resolve("daily-normal", &[KIMI, FLASH], None);
    let resolved = resolved(&result);
    assert_eq!(resolved.provider, "moonshotai");
    assert_eq!(resolved.model_id, "kimi-k3");
    assert_eq!(resolved.reasoning, Some("max"));
    assert_eq!(resolved.skipped, [OPUS_SUBSCRIPTION.to_owned()]);
    assert_eq!(profile_of(&result).id, "daily-normal");
    assert_eq!(profile_of(&result).source, ModelProfileSource::Builtin);
}

#[test]
fn resolves_a_rung_through_its_second_provider_when_the_first_is_absent() {
    let result = resolve("daily-normal", &[OPUS_API], None);
    let resolved = resolved(&result);
    assert_eq!(resolved.provider, "anthropic-api");
    assert_eq!(resolved.model_id, "");
    assert!(resolved.skipped.is_empty());
}

#[test]
fn treats_a_value_carrying_a_slash_as_a_literal_pin() {
    let result = resolve(OPUS, &[FABLE, OPUS], None);
    assert_eq!(result, expected_resolved(OPUS, OPUS, ModelProfileSource::Pin, "anthropic", "", None, &[]));
}

#[test]
fn reports_a_pin_the_registry_cannot_serve_as_unavailable() {
    let result = resolve(OPUS, &[FLASH], None);
    assert_eq!(
        result,
        ModelProfileResolution::Unavailable {
            profile: ModelProfileSummary {
                id: OPUS.to_owned(),
                display_name: OPUS.to_owned(),
                source: ModelProfileSource::Pin,
                family: None,
                tier: None,
            },
            chain: vec![OPUS.to_owned()],
        }
    );
}

#[test]
fn lets_a_user_entry_replace_a_builtin_of_the_same_name_wholesale() {
    let result = resolve(
        "daily-normal",
        &[OPUS_SUBSCRIPTION, FLASH],
        Some(json!({"daily-normal": {"models": [FLASH]}})),
    );
    let resolved = resolved(&result);
    assert_eq!(resolved.provider, "deepseek");
    assert_eq!(resolved.model_id, "deepseek-flash");
    assert!(resolved.skipped.is_empty());
    let profile = profile_of(&result);
    assert_eq!(profile.id, "daily-normal");
    assert_eq!(profile.display_name, "Daily · Normal");
    assert_eq!(profile.source, ModelProfileSource::User);
    assert_eq!(profile.family.map(|family| family.as_str()), Some("daily"));
    assert_eq!(profile.tier.map(|tier| tier.as_str()), Some("normal"));
}

#[test]
fn reports_a_label_only_override_as_empty_instead_of_falling_back_to_the_builtin_chain() {
    let result = resolve(
        "daily-normal",
        &[OPUS_SUBSCRIPTION],
        Some(json!({"daily-normal": {"display_name": "House blend"}})),
    );
    assert!(matches!(result, ModelProfileResolution::Empty { .. }));
    let profile = profile_of(&result);
    assert_eq!(profile.display_name, "House blend");
    assert_eq!(profile.source, ModelProfileSource::User);
    assert_eq!(profile.family.map(|family| family.as_str()), Some("daily"));
    assert_eq!(profile.tier.map(|tier| tier.as_str()), Some("normal"));
}

#[test]
fn resolves_a_profile_the_user_added() {
    let result = resolve(
        "night-shift",
        &[LUNA],
        Some(json!({"night-shift": {"display_name": "Night shift", "models": [{"model": "gpt-5.6-luna-fast", "reasoning": "low"}]}})),
    );
    let resolved = resolved(&result);
    assert_eq!(resolved.provider, "openai");
    assert_eq!(resolved.model_id, "gpt-5.6-luna-fast");
    assert_eq!(resolved.reasoning, Some("low"));
    let profile = profile_of(&result);
    assert_eq!(profile.display_name, "Night shift");
    assert_eq!(profile.source, ModelProfileSource::User);
    assert_eq!(profile.family, None);
}

#[test]
fn carries_a_reasoning_suffix_written_on_a_user_chain_entry() {
    let result = resolve("pair", &[OPUS], Some(json!({"pair": {"models": [format!("{OPUS}:high")]}})));
    let resolved = resolved(&result);
    assert_eq!(resolved.provider, "anthropic");
    assert_eq!(resolved.model_id, "");
    assert_eq!(resolved.reasoning, Some("high"));
}

#[test]
fn reports_an_empty_registry_as_unavailable_and_lists_the_chain() {
    let result = resolve("daily-normal", &[], None);
    assert_eq!(
        chain_of(&result),
        [OPUS_SUBSCRIPTION.to_owned(), "kimi-coding/kimi-k3".to_owned(), "zai/glm-5.3".to_owned()]
    );
}

#[test]
fn reports_an_unknown_name_once_with_the_known_profiles_sorted() {
    let result = resolve("nope", &[LUNA], Some(json!({"night-shift": {"models": [LUNA]}})));
    let ModelProfileResolution::Unknown { name, known, message } = result else {
        panic!("expected unknown");
    };
    assert_eq!(name, "nope");
    assert_eq!(known, ["daily-heavy", "daily-normal", "geeky-heavy", "geeky-normal", "night-shift", "recommended"]);
    assert_eq!(
        message,
        "model_profile \"nope\" is not defined; known profiles: daily-heavy, daily-normal, geeky-heavy, geeky-normal, night-shift, recommended"
    );
}

#[test]
fn reports_a_blank_value_as_unknown_rather_than_silently_doing_nothing() {
    let result = resolve("   ", &[LUNA], None);
    let ModelProfileResolution::Unknown { name, known, .. } = result else {
        panic!("expected unknown");
    };
    assert_eq!(name, "");
    assert_eq!(known, ["daily-heavy", "daily-normal", "geeky-heavy", "geeky-normal", "recommended"]);
}

#[test]
fn keeps_daily_heavy_on_the_claude_subscription_when_an_opencode_zen_key_serves_the_same_model() {
    let result = resolve("daily-heavy", &[FABLE_ZEN, FABLE], None);
    let resolved = resolved(&result);
    assert_eq!(resolved.provider, "anthropic-subscription");
    assert_eq!(resolved.model_id, "");
    assert_eq!(resolved.reasoning, Some("xhigh"));
}

#[test]
fn keeps_daily_normal_on_the_claude_subscription_when_zen_also_serves_opus() {
    let result = resolve("daily-normal", &[OPUS_ZEN, OPUS_SUBSCRIPTION], None);
    let resolved = resolved(&result);
    assert_eq!(resolved.provider, "anthropic-subscription");
    assert_eq!(resolved.model_id, "");
    assert_eq!(resolved.reasoning, Some("medium"));
}

#[test]
fn reports_the_removed_capable_deep_work_and_simple_work_ids_as_unknown() {
    let available = [OPUS_SUBSCRIPTION, ASTRA];
    for name in ["capable", "deep-work", "simple-work"] {
        let result = resolve(name, &available, None);
        let ModelProfileResolution::Unknown { name: reported, known, .. } = result else {
            panic!("expected unknown for {name}");
        };
        assert_eq!(reported, name);
        assert_eq!(known, ["daily-heavy", "daily-normal", "geeky-heavy", "geeky-normal", "recommended"]);
    }
}

#[test]
fn resolves_geeky_normal_to_gpt_61_sol_medium_when_the_subscription_serves_it_next_to_gpt_56_sol() {
    let result = resolve("geeky-normal", &[SOL_56, SOL_61], None);
    let resolved = resolved(&result);
    assert_eq!(resolved.provider, "chatgpt-subscription");
    assert_eq!(resolved.model_id, "gpt-6.1-sol");
    assert_eq!(resolved.reasoning, Some("medium"));
}

#[test]
fn resolves_geeky_normal_to_gpt_56_sol_medium_when_only_copilot_serves_it() {
    let result = resolve("geeky-normal", &[SOL_56_COPILOT], None);
    let resolved = resolved(&result);
    assert_eq!(resolved.provider, "github-copilot");
    assert_eq!(resolved.model_id, "gpt-5.6-sol");
    assert_eq!(resolved.reasoning, Some("medium"));
}

#[test]
fn prefers_the_chatgpt_subscription_lane_over_copilot_for_geeky_normal_gpt_56_sol() {
    let result = resolve("geeky-normal", &[SOL_56_COPILOT, SOL_56], None);
    let resolved = resolved(&result);
    assert_eq!(resolved.provider, "chatgpt-subscription");
    assert_eq!(resolved.model_id, "gpt-5.6-sol");
    assert_eq!(resolved.reasoning, Some("medium"));
}

#[test]
fn does_not_fall_back_to_a_gpt_6_model_when_geeky_normals_gpt_56_sol_is_missing() {
    let result = resolve("geeky-normal", &[SOL_COPILOT, SOL_FAST], None);
    assert!(matches!(result, ModelProfileResolution::Unavailable { .. }));
}

#[test]
fn ranks_the_openai_api_lane_ahead_of_an_unlisted_provider_serving_geeky_normal_gpt_56_sol() {
    let result = resolve("geeky-normal", &["office-gateway/gpt-5.6-sol", "openai/gpt-5.6-sol"], None);
    let resolved = resolved(&result);
    assert_eq!(resolved.provider, "openai");
    assert_eq!(resolved.model_id, "gpt-5.6-sol");
    assert_eq!(resolved.reasoning, Some("medium"));
}

#[test]
fn never_serves_a_builtin_lane_from_a_gateways_copy_of_its_models() {
    for active in ["daily-normal", "daily-heavy", "geeky-normal", "geeky-heavy"] {
        let result = resolve(
            active,
            &[
                "openrouter/anthropic/",
                "opengateway/anthropic/",
                "openrouter/moonshotai/kimi-k3",
                "openrouter/openai/gpt-6-astra",
                "openrouter/openai/gpt-5.6-sol",
                "office-gateway/gpt-5.6-sol",
            ],
            None,
        );
        assert!(matches!(result, ModelProfileResolution::Unavailable { .. }), "{active} stays unavailable");
    }
}

#[test]
fn lets_a_users_bare_model_id_which_names_no_provider_match_a_gateway() {
    let result = resolve(
        "mine",
        &["office-gateway/gpt-5.6-sol"],
        Some(json!({"mine": {"models": ["gpt-5.6-sol"]}})),
    );
    let resolved = resolved(&result);
    assert_eq!(resolved.provider, "office-gateway");
    assert_eq!(resolved.model_id, "gpt-5.6-sol");
}

#[test]
fn keeps_the_chatgpt_subscription_ahead_of_the_openai_api_lane_for_geeky_heavy_astra() {
    let result = resolve("geeky-heavy", &["openai/gpt-6-astra", ASTRA], None);
    let resolved = resolved(&result);
    assert_eq!(resolved.provider, "chatgpt-subscription");
    assert_eq!(resolved.model_id, "gpt-6-astra");
    assert_eq!(resolved.reasoning, Some("high"));
}

#[test]
fn resolves_geeky_heavy_to_astra_high() {
    let result = resolve("geeky-heavy", &[ASTRA, SOL_FAST], None);
    let resolved = resolved(&result);
    assert_eq!(resolved.model_id, "gpt-6-astra");
    assert_eq!(resolved.reasoning, Some("high"));
}

#[test]
fn does_not_silently_take_another_provider_when_a_user_overlay_names_openai_and_openai_is_absent() {
    let result = resolve(
        "geeky-normal",
        &["chatgpt-subscription/gpt-6-sol"],
        Some(json!({"geeky-normal": {"models": [{"model": "openai/gpt-6-sol", "reasoning": "medium"}]}})),
    );
    assert!(matches!(result, ModelProfileResolution::Unavailable { .. }));
}

#[test]
fn walks_only_the_user_listed_providers_when_the_first_scoped_rung_is_missing() {
    let result = resolve(
        "geeky-normal",
        &[SOL_COPILOT, SOL_FAST],
        Some(json!({"geeky-normal": {"models": [
            {"model": "openai/gpt-6-sol", "reasoning": "high"},
            {"model": "github-copilot/gpt-6-sol", "reasoning": "medium"}
        ]}})),
    );
    let resolved = resolved(&result);
    assert_eq!(resolved.provider, "github-copilot");
    assert_eq!(resolved.model_id, "gpt-6-sol");
    assert_eq!(resolved.reasoning, Some("medium"));
}

#[test]
fn still_matches_an_unscoped_user_model_through_whichever_registry_provider_serves_it() {
    let result = resolve(
        "geeky-normal",
        &["chatgpt-subscription/gpt-6-sol"],
        Some(json!({"geeky-normal": {"models": [{"model": "gpt-6-sol", "reasoning": "medium"}]}})),
    );
    let resolved = resolved(&result);
    assert_eq!(resolved.provider, "chatgpt-subscription");
    assert_eq!(resolved.model_id, "gpt-6-sol");
    assert_eq!(resolved.reasoning, Some("medium"));
}

#[test]
fn selects_the_named_provider_when_a_scoped_user_rung_is_present() {
    let result = resolve(
        "geeky-normal",
        &["openai/gpt-6-sol", "chatgpt-subscription/gpt-6-sol"],
        Some(json!({"geeky-normal": {"models": [{"model": "openai/gpt-6-sol", "reasoning": "high"}]}})),
    );
    let resolved = resolved(&result);
    assert_eq!(resolved.provider, "openai");
    assert_eq!(resolved.model_id, "gpt-6-sol");
    assert_eq!(resolved.reasoning, Some("high"));
}

#[test]
fn keeps_geeky_normal_family_tier_when_a_user_replaces_the_chain_with_an_openai_model_and_does_not_merge_builtin_rungs() {
    let result = resolve(
        "geeky-normal",
        &[SOL_FAST, "openai/gpt-6-sol"],
        Some(json!({"geeky-normal": {"models": [{"model": "openai/gpt-6-sol", "reasoning": "high"}]}})),
    );
    let profile = profile_of(&result);
    assert_eq!(profile.id, "geeky-normal");
    assert_eq!(profile.display_name, "Geeky · Normal");
    assert_eq!(profile.source, ModelProfileSource::User);
    assert_eq!(profile.family.map(|family| family.as_str()), Some("geeky"));
    assert_eq!(profile.tier.map(|tier| tier.as_str()), Some("normal"));
    let resolved = resolved(&result);
    assert_eq!(resolved.provider, "openai");
    assert_eq!(resolved.model_id, "gpt-6-sol");
    assert_eq!(resolved.reasoning, Some("high"));
    assert!(resolved.skipped.is_empty());
}

#[test]
fn resolves_recommended_to_gpt_61_sol_medium_when_the_subscription_serves_it_next_to_gpt_6_sol() {
    let result = resolve("recommended", &["chatgpt-subscription/gpt-6-sol", SOL_61, "zai/glm-5.3"], None);
    let resolved = resolved(&result);
    assert_eq!(resolved.provider, "chatgpt-subscription");
    assert_eq!(resolved.model_id, "gpt-6.1-sol");
    assert_eq!(resolved.reasoning, Some("medium"));
}

#[test]
fn resolves_recommended_to_gpt_6_sol_medium_when_only_copilot_serves_a_gpt_6_sol() {
    let result = resolve("recommended", &[SOL_COPILOT, "zai/glm-5.3"], None);
    let resolved = resolved(&result);
    assert_eq!(resolved.provider, "github-copilot");
    assert_eq!(resolved.model_id, "gpt-6-sol");
    assert_eq!(resolved.reasoning, Some("medium"));
}

#[test]
fn picks_zai_glm_53_on_recommended_when_only_a_zai_key_serves_glm() {
    let result = resolve("recommended", &["zai/glm-5.3"], None);
    let resolved = resolved(&result);
    assert_eq!(resolved.provider, "zai");
    assert_eq!(resolved.model_id, "glm-5.3");
    assert_eq!(resolved.reasoning, Some("max"));
}

#[test]
fn picks_zai_glm_53_on_daily_normal_when_only_a_zai_key_serves_glm() {
    let result = resolve("daily-normal", &["zai/glm-5.3"], None);
    let resolved = resolved(&result);
    assert_eq!(resolved.provider, "zai");
    assert_eq!(resolved.model_id, "glm-5.3");
    assert_eq!(resolved.reasoning, Some("max"));
}

#[test]
fn does_not_pick_opencode_zai_coding_plan_on_recommended() {
    let result = resolve("recommended", &["zai-coding-plan/glm-5.3"], None);
    assert!(matches!(result, ModelProfileResolution::Unavailable { .. }));
}

#[test]
fn a_pin_is_not_probed_for_auth_and_keeps_the_users_explicit_choice() {
    let result = resolve(OPUS, &[OPUS, FABLE], None);
    let profile = profile_of(&result);
    assert_eq!(profile.source, ModelProfileSource::Pin);
    assert_eq!(profile.display_name, profile.id);
}

#[test]
fn a_profile_whose_registry_serves_nothing_reports_the_whole_chain() {
    let result = resolve("geeky-heavy", &["example/nothing"], None);
    assert_eq!(chain_of(&result), ["chatgpt-subscription/gpt-6-astra".to_owned()]);
}
