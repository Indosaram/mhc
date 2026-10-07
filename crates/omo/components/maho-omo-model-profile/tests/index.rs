mod support;

use std::sync::Arc;

use maho_ai::types::ModelThinkingLevel;
use maho_ext_api::{
    CustomMessage, EventKind, EventResult, ExtensionEvent, ExtensionMode, ProviderAuthStatus,
    SessionReason,
};
use maho_omo_model_profile::index::{
    MODEL_PROFILE_APPLIED_TYPE, MODEL_PROFILE_UNAVAILABLE_TYPE, MODEL_PROFILE_UNKNOWN_TYPE,
    ModelProfileComponent,
};
use serde_json::json;
use support::{PRIVATE_MARKER, TestRegistry, model};

const OPUS: (&str, &str) = ("anthropic", "");
const SUBSCRIPTION_OPUS: (&str, &str) = ("anthropic-subscription", "");
const KIMI: (&str, &str) = ("moonshotai", "kimi-k3");
const CODING_KIMI: (&str, &str) = ("kimi-coding", "kimi-k3");
const GLM: (&str, &str) = ("zai", "glm-5.3");
const GATEWAY_OPUS: (&str, &str) = ("opengateway", "anthropic/");
const SOL: (&str, &str) = ("github-copilot", "gpt-6-sol");
const SOL_56_COPILOT: (&str, &str) = ("github-copilot", "gpt-5.6-sol");
const SOL_61: (&str, &str) = ("chatgpt-subscription", "gpt-6.1-sol");
const SOL_61_FAST: (&str, &str) = ("chatgpt-subscription", "gpt-6.1-sol-fast");
const SOL_FAST: (&str, &str) = ("chatgpt-subscription", "gpt-6-sol-fast");
const ASTRA: (&str, &str) = ("chatgpt-subscription", "gpt-6-astra");
const UNRELATED: (&str, &str) = ("example", "nothing-in-any-chain");

fn fixture(pairs: &[(&str, &str)]) -> Vec<maho_ai::model::Model> {
    pairs.iter().map(|(provider, id)| model(provider, id)).collect()
}

fn component(config: serde_json::Value) -> ModelProfileComponent {
    ModelProfileComponent::with_load_config(Arc::new(move |_| support::config_result(config.clone())))
}

fn harness_for(
    config: serde_json::Value,
    registry: Arc<TestRegistry>,
    session_id: &str,
    mode: ExtensionMode,
) -> support::Harness {
    let dir = tempfile::tempdir().expect("temp dir");
    support::harness_in(component(config), registry, session_id, mode, "/project", dir)
}

async fn start(
    config: serde_json::Value,
    registry: Arc<TestRegistry>,
    session_id: &str,
    mode: ExtensionMode,
    reason: SessionReason,
    provenance: Option<&str>,
) -> support::Harness {
    let harness = harness_for(config, registry, session_id, mode);
    harness.start(reason, provenance).await;
    harness
}

fn selectors(models: &[maho_ai::model::Model]) -> Vec<String> {
    models.iter().map(|model| format!("{}/{}", model.provider, model.id)).collect()
}

const STARTUP: SessionReason = SessionReason::Startup;

#[tokio::test]
async fn a_tui_session_applies_nothing_for_unset_lane_and_pin_configs() {
    for config in [json!({}), json!({"model_profile": "daily-heavy"}), json!({"model_profile": "anthropic/"})] {
        let harness = start(
            config,
            Arc::new(TestRegistry::new(fixture(&[SUBSCRIPTION_OPUS, KIMI]))),
            "session-tui",
            ExtensionMode::Tui,
            STARTUP,
            Some("settings"),
        )
        .await;
        assert!(harness.session_models().is_empty());
        assert!(harness.session_thinking_levels().is_empty());
        assert!(harness.messages().is_empty());
    }
}

#[tokio::test]
async fn an_unset_model_profile_applies_the_recommended_ladder() {
    let harness = start(
        json!({}),
        Arc::new(TestRegistry::new(fixture(&[OPUS, KIMI]))),
        "session-1",
        ExtensionMode::Rpc,
        STARTUP,
        Some("settings"),
    )
    .await;
    assert_eq!(selectors(&harness.session_models()), ["anthropic/"]);
    assert_eq!(harness.session_thinking_levels(), [ModelThinkingLevel::Medium]);
    assert_eq!(harness.first_custom_type(), MODEL_PROFILE_APPLIED_TYPE);
    assert_eq!(harness.first_details()["profile"], json!("recommended"));
    assert_eq!(harness.first_details()["model"], json!("anthropic/"));
    assert_eq!(harness.first_details()["reasoning"], json!("medium"));
}

#[tokio::test]
async fn a_blank_model_profile_applies_the_recommended_ladder() {
    let harness = start(
        json!({"model_profile": "   "}),
        Arc::new(TestRegistry::new(fixture(&[OPUS]))),
        "session-1",
        ExtensionMode::Rpc,
        STARTUP,
        Some("settings"),
    )
    .await;
    assert_eq!(selectors(&harness.session_models()), ["anthropic/"]);
    assert_eq!(harness.first_details()["profile"], json!("recommended"));
}

#[tokio::test]
async fn a_gateway_only_opus_is_skipped_for_the_next_rung() {
    let harness = start(
        json!({}),
        Arc::new(TestRegistry::new(fixture(&[GATEWAY_OPUS, CODING_KIMI]))),
        "session-1",
        ExtensionMode::Rpc,
        STARTUP,
        Some("settings"),
    )
    .await;
    assert_eq!(selectors(&harness.session_models()), ["kimi-coding/kimi-k3"]);
    assert_eq!(harness.session_thinking_levels(), [ModelThinkingLevel::Max]);
    assert_eq!(harness.first_details()["model"], json!("kimi-coding/kimi-k3"));
}

#[tokio::test]
async fn the_claude_subscription_lane_wins_over_the_api_lane() {
    let harness = start(
        json!({}),
        Arc::new(TestRegistry::new(fixture(&[OPUS, SUBSCRIPTION_OPUS, KIMI]))),
        "session-1",
        ExtensionMode::Rpc,
        STARTUP,
        Some("settings"),
    )
    .await;
    assert_eq!(selectors(&harness.session_models()), ["anthropic-subscription/"]);
}

#[tokio::test]
async fn gpt_61_sol_medium_is_applied_ahead_of_glm() {
    let harness = start(
        json!({}),
        Arc::new(TestRegistry::new(fixture(&[SOL, SOL_61, GLM, UNRELATED]))),
        "session-1",
        ExtensionMode::Rpc,
        STARTUP,
        Some("settings"),
    )
    .await;
    assert_eq!(selectors(&harness.session_models()), ["chatgpt-subscription/gpt-6.1-sol"]);
    assert_eq!(harness.session_thinking_levels(), [ModelThinkingLevel::Medium]);
}

#[tokio::test]
async fn copilot_gpt_6_sol_medium_is_the_fallback_rung() {
    let harness = start(
        json!({}),
        Arc::new(TestRegistry::new(fixture(&[SOL, GLM, UNRELATED]))),
        "session-1",
        ExtensionMode::Rpc,
        STARTUP,
        Some("settings"),
    )
    .await;
    assert_eq!(selectors(&harness.session_models()), ["github-copilot/gpt-6-sol"]);
}

#[tokio::test]
async fn daily_normal_skips_a_gateway_for_its_next_listed_rung() {
    let harness = start(
        json!({"model_profile": "daily-normal"}),
        Arc::new(TestRegistry::new(fixture(&[GATEWAY_OPUS, CODING_KIMI]))),
        "session-1",
        ExtensionMode::Rpc,
        STARTUP,
        Some("settings"),
    )
    .await;
    assert_eq!(selectors(&harness.session_models()), ["kimi-coding/kimi-k3"]);
}

#[tokio::test]
async fn daily_normal_with_only_a_gateway_keeps_the_session_model_with_one_unavailable_notice() {
    let harness = start(
        json!({"model_profile": "daily-normal"}),
        Arc::new(TestRegistry::new(fixture(&[GATEWAY_OPUS]))),
        "session-1",
        ExtensionMode::Rpc,
        STARTUP,
        Some("settings"),
    )
    .await;
    assert!(harness.session_models().is_empty());
    assert_eq!(harness.messages().len(), 1);
    assert_eq!(harness.first_custom_type(), MODEL_PROFILE_UNAVAILABLE_TYPE);
    assert!(harness.messages()[0].display);
}

#[tokio::test]
async fn daily_normal_with_only_the_third_rung_applies_kimi_max_and_names_the_skipped_rungs() {
    let harness = start(
        json!({"model_profile": "daily-normal"}),
        Arc::new(TestRegistry::new(fixture(&[KIMI, UNRELATED]))),
        "session-1",
        ExtensionMode::Rpc,
        STARTUP,
        Some("settings"),
    )
    .await;
    assert_eq!(selectors(&harness.session_models()), ["moonshotai/kimi-k3"]);
    assert_eq!(harness.session_thinking_levels(), [ModelThinkingLevel::Max]);
    assert_eq!(harness.first_details()["skipped"], json!(["anthropic-subscription/"]));
    assert!(!harness.agent_dir.join("settings.json").exists());
}

#[tokio::test]
async fn daily_heavy_applies_fable_xhigh() {
    let harness = start(
        json!({"model_profile": "daily-heavy"}),
        Arc::new(TestRegistry::new(fixture(&[SUBSCRIPTION_OPUS, KIMI]))),
        "session-1",
        ExtensionMode::Rpc,
        STARTUP,
        Some("settings"),
    )
    .await;
    assert_eq!(selectors(&harness.session_models()), ["anthropic-subscription/"]);
    assert_eq!(harness.session_thinking_levels(), [ModelThinkingLevel::Xhigh]);
}

#[tokio::test]
async fn geeky_normal_applies_gpt_61_sol_fast_medium() {
    let harness = start(
        json!({"model_profile": "geeky-normal"}),
        Arc::new(TestRegistry::new(fixture(&[SOL_61, SOL_61_FAST, SOL_56_COPILOT, UNRELATED]))),
        "session-1",
        ExtensionMode::Rpc,
        STARTUP,
        Some("settings"),
    )
    .await;
    assert_eq!(selectors(&harness.session_models()), ["chatgpt-subscription/gpt-6.1-sol-fast"]);
    assert_eq!(harness.session_thinking_levels(), [ModelThinkingLevel::Medium]);
}

#[tokio::test]
async fn geeky_normal_without_the_fast_tier_applies_gpt_61_sol_medium() {
    let harness = start(
        json!({"model_profile": "geeky-normal"}),
        Arc::new(TestRegistry::new(fixture(&[SOL_56_COPILOT, SOL_61, UNRELATED]))),
        "session-1",
        ExtensionMode::Rpc,
        STARTUP,
        Some("settings"),
    )
    .await;
    assert_eq!(selectors(&harness.session_models()), ["chatgpt-subscription/gpt-6.1-sol"]);
}

#[tokio::test]
async fn geeky_normal_with_only_copilot_applies_gpt_56_sol_medium() {
    let harness = start(
        json!({"model_profile": "geeky-normal"}),
        Arc::new(TestRegistry::new(fixture(&[SOL_56_COPILOT, SOL, UNRELATED]))),
        "session-1",
        ExtensionMode::Rpc,
        STARTUP,
        Some("settings"),
    )
    .await;
    assert_eq!(selectors(&harness.session_models()), ["github-copilot/gpt-5.6-sol"]);
}

#[tokio::test]
async fn geeky_heavy_applies_astra_high() {
    let harness = start(
        json!({"model_profile": "geeky-heavy"}),
        Arc::new(TestRegistry::new(fixture(&[ASTRA, SOL_FAST]))),
        "session-1",
        ExtensionMode::Rpc,
        STARTUP,
        Some("settings"),
    )
    .await;
    assert_eq!(selectors(&harness.session_models()), ["chatgpt-subscription/gpt-6-astra"]);
    assert_eq!(harness.session_thinking_levels(), [ModelThinkingLevel::High]);
}

#[tokio::test]
async fn a_literal_provider_model_pin_is_applied_for_the_session() {
    let harness = start(
        json!({"model_profile": "anthropic/"}),
        Arc::new(TestRegistry::new(fixture(&[OPUS]))),
        "session-1",
        ExtensionMode::Rpc,
        STARTUP,
        Some("settings"),
    )
    .await;
    assert_eq!(selectors(&harness.session_models()), ["anthropic/"]);
    assert!(harness.session_thinking_levels().is_empty());
    assert_eq!(harness.first_custom_type(), MODEL_PROFILE_APPLIED_TYPE);
    assert!(harness.registry.calls().is_empty(), "a pin is never probed");
}

#[tokio::test]
async fn a_lane_with_no_available_rung_makes_no_model_call_and_emits_an_unavailable_notice() {
    let harness = start(
        json!({"model_profile": "daily-normal"}),
        Arc::new(TestRegistry::new(fixture(&[UNRELATED]))),
        "session-1",
        ExtensionMode::Rpc,
        STARTUP,
        Some("settings"),
    )
    .await;
    assert!(harness.session_models().is_empty());
    assert!(harness.session_thinking_levels().is_empty());
    assert_eq!(harness.first_custom_type(), MODEL_PROFILE_UNAVAILABLE_TYPE);
    assert_eq!(harness.first_details(), json!(null));
}

#[tokio::test]
async fn an_unknown_profile_name_emits_the_resolver_diagnostic_and_applies_nothing() {
    let harness = start(
        json!({"model_profile": "turbo"}),
        Arc::new(TestRegistry::new(fixture(&[OPUS]))),
        "session-1",
        ExtensionMode::Rpc,
        STARTUP,
        Some("settings"),
    )
    .await;
    assert!(harness.session_models().is_empty());
    assert_eq!(harness.messages().len(), 1);
    assert_eq!(harness.first_custom_type(), MODEL_PROFILE_UNKNOWN_TYPE);
    assert!(harness.logger.lines().iter().any(|line| line.starts_with("warn:")));
}

#[tokio::test]
async fn retired_capable_and_deep_work_ids_are_unknown() {
    for name in ["capable", "deep-work", "simple-work"] {
        let harness = start(
            json!({"model_profile": name}),
            Arc::new(TestRegistry::new(fixture(&[OPUS, ASTRA]))),
            "session-1",
            ExtensionMode::Rpc,
            STARTUP,
            Some("settings"),
        )
        .await;
        assert!(harness.session_models().is_empty(), "{name} applies nothing");
        assert_eq!(harness.first_custom_type(), MODEL_PROFILE_UNKNOWN_TYPE);
    }
}

#[tokio::test]
async fn a_user_overlay_with_an_openai_model_applies_that_provider_and_reasoning_without_merging_builtin_rungs() {
    let harness = start(
        json!({
            "model_profile": "geeky-normal",
            "model_profiles": {"geeky-normal": {"display_name": "Office GPT", "models": [{"model": "openai/gpt-6-sol", "reasoning": "high"}]}}
        }),
        Arc::new(TestRegistry::new(fixture(&[("openai", "gpt-6-sol"), SOL_FAST]))),
        "session-1",
        ExtensionMode::Rpc,
        STARTUP,
        Some("settings"),
    )
    .await;
    assert_eq!(selectors(&harness.session_models()), ["openai/gpt-6-sol"]);
    assert_eq!(harness.session_thinking_levels(), [ModelThinkingLevel::High]);
    assert_eq!(harness.first_details()["skipped"], json!([]));
}

#[tokio::test]
async fn a_custom_user_profile_chain_is_applied() {
    let harness = start(
        json!({
            "model_profile": "night-shift",
            "model_profiles": {"night-shift": {"display_name": "Night shift", "models": ["moonshotai/kimi-k3"]}}
        }),
        Arc::new(TestRegistry::new(fixture(&[KIMI]))),
        "session-1",
        ExtensionMode::Rpc,
        STARTUP,
        Some("settings"),
    )
    .await;
    assert_eq!(selectors(&harness.session_models()), ["moonshotai/kimi-k3"]);
    assert_eq!(harness.first_details()["profile"], json!("night-shift"));
}

#[tokio::test]
async fn a_resumed_forked_or_reloaded_session_is_not_applied() {
    let harness = harness_for(
        json!({"model_profile": "daily-normal"}),
        Arc::new(TestRegistry::new(fixture(&[SUBSCRIPTION_OPUS]))),
        "session-1",
        ExtensionMode::Rpc,
    );
    harness.start(SessionReason::Resume, Some("settings")).await;
    harness.start(SessionReason::Fork, Some("settings")).await;
    harness.start(SessionReason::Reload, None).await;
    harness.start(SessionReason::Quit, None).await;
    assert!(harness.session_models().is_empty());
    assert!(harness.messages().is_empty());
}

#[tokio::test]
async fn a_cli_or_scoped_model_yields_to_the_profile() {
    let harness = harness_for(
        json!({"model_profile": "daily-normal"}),
        Arc::new(TestRegistry::new(fixture(&[SUBSCRIPTION_OPUS]))),
        "session-1",
        ExtensionMode::Rpc,
    );
    harness.start(SessionReason::Startup, Some("cli")).await;
    harness.start(SessionReason::New, Some("scoped")).await;
    assert!(harness.session_models().is_empty());
    assert!(harness.messages().is_empty());
}

#[tokio::test]
async fn a_session_start_with_no_provenance_is_treated_as_explicit() {
    let harness = start(
        json!({"model_profile": "daily-normal"}),
        Arc::new(TestRegistry::new(fixture(&[SUBSCRIPTION_OPUS]))),
        "session-1",
        ExtensionMode::Rpc,
        STARTUP,
        None,
    )
    .await;
    assert!(harness.session_models().is_empty());
    assert!(harness.messages().is_empty());
}

#[tokio::test]
async fn two_session_starts_for_one_session_id_apply_once() {
    let harness = harness_for(
        json!({"model_profile": "daily-normal"}),
        Arc::new(TestRegistry::new(fixture(&[SUBSCRIPTION_OPUS]))),
        "session-1",
        ExtensionMode::Rpc,
    );
    harness.start(STARTUP, Some("settings")).await;
    harness.start(SessionReason::New, Some("settings")).await;
    assert_eq!(harness.session_models().len(), 1);
    assert_eq!(harness.messages().len(), 1);
}

#[tokio::test]
async fn a_new_session_id_applies_again() {
    let harness = harness_for(
        json!({"model_profile": "daily-normal"}),
        Arc::new(TestRegistry::new(fixture(&[SUBSCRIPTION_OPUS]))),
        "session-1",
        ExtensionMode::Rpc,
    );
    harness.start(STARTUP, Some("settings")).await;
    let second = support::harness_in(
        component(json!({"model_profile": "daily-normal"})),
        Arc::new(TestRegistry::new(fixture(&[SUBSCRIPTION_OPUS]))),
        "session-2",
        ExtensionMode::Rpc,
        "/project",
        tempfile::tempdir().expect("temp dir"),
    );
    second.start(STARTUP, Some("settings")).await;
    assert_eq!(second.session_models().len(), 1);
}

#[tokio::test]
async fn a_rejected_refresh_on_desktop_skips_the_provider_and_applies_the_next_healthy_rung() {
    let harness = start(
        json!({}),
        Arc::new(TestRegistry::new(fixture(&[OPUS, GLM])).dead_provider("anthropic")),
        "session-1",
        ExtensionMode::Rpc,
        STARTUP,
        Some("settings"),
    )
    .await;
    assert_eq!(selectors(&harness.session_models()), ["zai/glm-5.3"]);
    assert_eq!(harness.registry.calls(), ["anthropic", "zai", "zai/glm-5.3"]);
    assert_eq!(
        harness.first_details()["authFailed"],
        json!([{"provider": "anthropic", "model": "", "reason": "refresh"}])
    );
    let lines = harness.logger.lines();
    assert!(lines.iter().any(|line| line.starts_with("warn:") && line.contains("skipped anthropic/")));
    assert!(!lines.iter().any(|line| line.contains(PRIVATE_MARKER)));
}

#[tokio::test]
async fn a_rejected_refresh_on_a_headless_run_still_reports_the_interactive_login_guidance() {
    let harness = start(
        json!({}),
        Arc::new(TestRegistry::new(fixture(&[OPUS, GLM])).dead_provider("anthropic")),
        "session-print",
        ExtensionMode::Print,
        STARTUP,
        Some("settings"),
    )
    .await;
    assert_eq!(selectors(&harness.session_models()), ["zai/glm-5.3"]);
    let lines = harness.logger.lines();
    assert!(lines.iter().any(|line| line.starts_with("warn:")));
    assert!(!lines.iter().any(|line| line.contains(PRIVATE_MARKER)));
}

#[tokio::test]
async fn two_distinct_rejected_providers_are_skipped_before_a_healthy_one() {
    let harness = start(
        json!({}),
        Arc::new(
            TestRegistry::new(fixture(&[OPUS, KIMI, GLM]))
                .dead_provider("anthropic")
                .dead_provider("moonshotai"),
        ),
        "session-1",
        ExtensionMode::Rpc,
        STARTUP,
        Some("settings"),
    )
    .await;
    assert_eq!(selectors(&harness.session_models()), ["zai/glm-5.3"]);
    assert_eq!(harness.registry.calls(), ["anthropic", "moonshotai", "zai", "zai/glm-5.3"]);
    assert_eq!(
        harness.first_details()["authFailed"],
        json!([
            {"provider": "anthropic", "model": "", "reason": "refresh"},
            {"provider": "moonshotai", "model": "kimi-k3", "reason": "refresh"}
        ])
    );
}

#[tokio::test]
async fn every_provider_failing_credentials_emits_one_unavailable_notice_listing_each() {
    let harness = start(
        json!({}),
        Arc::new(TestRegistry::new(fixture(&[OPUS, KIMI])).dead_provider("anthropic").dead_provider("moonshotai")),
        "session-1",
        ExtensionMode::Rpc,
        STARTUP,
        Some("settings"),
    )
    .await;
    assert!(harness.session_models().is_empty());
    assert!(harness.session_thinking_levels().is_empty());
    assert_eq!(harness.registry.calls(), ["anthropic", "moonshotai"]);
    assert_eq!(harness.first_custom_type(), MODEL_PROFILE_UNAVAILABLE_TYPE);
    assert_eq!(harness.first_details()["profile"], json!("recommended"));
    assert_eq!(harness.first_details()["authFailed"].as_array().map(Vec::len), Some(2));
    assert!(!harness.logger.lines().iter().any(|line| line.contains(PRIVATE_MARKER)));
}

#[tokio::test]
async fn a_failing_model_request_configuration_skips_only_that_candidate_and_the_walk_continues() {
    let harness = start(
        json!({}),
        Arc::new(TestRegistry::new(fixture(&[OPUS, GLM])).broken_model("anthropic", "")),
        "session-1",
        ExtensionMode::Rpc,
        STARTUP,
        Some("settings"),
    )
    .await;
    assert_eq!(selectors(&harness.session_models()), ["zai/glm-5.3"]);
    assert_eq!(harness.registry.calls(), ["anthropic", "anthropic/", "zai", "zai/glm-5.3"]);
    assert_eq!(
        harness.first_details()["authFailed"],
        json!([{"provider": "anthropic", "model": "", "reason": "request"}])
    );
}

#[tokio::test]
async fn rotation_disabled_in_models_json_probes_only_the_flat_credential() {
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::write(
        dir.path().join("models.json"),
        "{\n  // JSONC, as senpi reads it\n  \"providers\": { \"anthropic\": { \"credentials\": { \"rotation\": false } } }\n}\n",
    )
    .expect("models.json");
    let harness = support::harness_in(
        component(json!({})),
        Arc::new(
            TestRegistry::new(fixture(&[OPUS, GLM]))
                .dead_provider("anthropic")
                .with_accounts("anthropic", vec![support::account("expired", false), support::account("healthy", false)]),
        ),
        "session-1",
        ExtensionMode::Rpc,
        "/project",
        dir,
    );
    harness.start(STARTUP, Some("settings")).await;
    assert_eq!(selectors(&harness.session_models()), ["zai/glm-5.3"]);
    assert_eq!(harness.registry.calls(), ["anthropic", "zai", "zai/glm-5.3"]);
}

#[tokio::test]
async fn a_lane_whose_first_rung_resolves_credentials_applies_after_one_provider_and_one_model_probe() {
    let harness = start(
        json!({"model_profile": "daily-heavy"}),
        Arc::new(TestRegistry::new(fixture(&[SUBSCRIPTION_OPUS, OPUS, KIMI]))),
        "session-1",
        ExtensionMode::Rpc,
        STARTUP,
        Some("settings"),
    )
    .await;
    assert_eq!(selectors(&harness.session_models()), ["anthropic-subscription/"]);
    assert_eq!(harness.registry.calls(), ["anthropic-subscription", "anthropic-subscription/"]);
    assert_eq!(harness.first_details()["authFailed"], json!(null));
}

#[tokio::test]
async fn a_literal_pin_with_failing_credentials_is_applied_unprobed() {
    let harness = start(
        json!({"model_profile": "anthropic/"}),
        Arc::new(TestRegistry::new(fixture(&[OPUS, GLM])).dead_provider("anthropic")),
        "session-1",
        ExtensionMode::Rpc,
        STARTUP,
        Some("settings"),
    )
    .await;
    assert_eq!(selectors(&harness.session_models()), ["anthropic/"]);
    assert!(harness.registry.calls().is_empty());
}

#[tokio::test]
async fn a_glm_only_registry_applies_glm_max() {
    let harness = start(
        json!({}),
        Arc::new(TestRegistry::new(fixture(&[GLM, UNRELATED]))),
        "session-1",
        ExtensionMode::Rpc,
        STARTUP,
        Some("settings"),
    )
    .await;
    assert_eq!(selectors(&harness.session_models()), ["zai/glm-5.3"]);
    assert_eq!(harness.session_thinking_levels(), [ModelThinkingLevel::Max]);
}

#[tokio::test]
async fn other_events_never_touch_the_model() {
    let harness = harness_for(
        json!({"model_profile": "daily-normal"}),
        Arc::new(TestRegistry::new(fixture(&[SUBSCRIPTION_OPUS]))),
        "session-1",
        ExtensionMode::Rpc,
    );
    assert_eq!(harness.api.registered.handlers.keys().copied().collect::<Vec<_>>(), [EventKind::SessionStart]);
    let mut model_select = ExtensionEvent::AgentStart;
    let _ = harness.api.registered.handlers[&EventKind::SessionStart][0](&mut model_select, &harness.ctx).await;
    assert!(harness.session_models().is_empty());
}

#[tokio::test]
async fn the_applied_notice_carries_the_profile_model_and_skipped_selector_details() {
    let harness = start(
        json!({"model_profile": "daily-normal"}),
        Arc::new(TestRegistry::new(fixture(&[KIMI]))),
        "session-1",
        ExtensionMode::Rpc,
        STARTUP,
        Some("settings"),
    )
    .await;
    let details = harness.first_details();
    assert_eq!(details["profile"], json!("daily-normal"));
    assert_eq!(details["model"], json!("moonshotai/kimi-k3"));
    assert_eq!(details["reasoning"], json!("max"));
    assert_eq!(details["skipped"], json!(["anthropic-subscription/"]));
}

#[tokio::test]
async fn reasoning_none_maps_to_the_off_session_level() {
    let harness = start(
        json!({"model_profile": "pair", "model_profiles": {"pair": {"models": [{"model": "anthropic/", "reasoning": "none"}]}}}),
        Arc::new(TestRegistry::new(fixture(&[OPUS]))),
        "session-1",
        ExtensionMode::Rpc,
        STARTUP,
        Some("settings"),
    )
    .await;
    assert_eq!(selectors(&harness.session_models()), ["anthropic/"]);
    assert_eq!(harness.session_thinking_levels(), [ModelThinkingLevel::Off]);
}

#[tokio::test]
async fn reasoning_auto_leaves_the_session_level_alone() {
    let harness = start(
        json!({"model_profile": "pair", "model_profiles": {"pair": {"models": [{"model": "anthropic/", "reasoning": "auto"}]}}}),
        Arc::new(TestRegistry::new(fixture(&[OPUS]))),
        "session-1",
        ExtensionMode::Rpc,
        STARTUP,
        Some("settings"),
    )
    .await;
    assert_eq!(selectors(&harness.session_models()), ["anthropic/"]);
    assert!(harness.session_thinking_levels().is_empty());
}

#[tokio::test]
async fn an_unknown_reasoning_token_leaves_the_session_level_alone() {
    let harness = start(
        json!({"model_profile": "pair", "model_profiles": {"pair": {"models": [{"model": "anthropic/", "reasoning": "ludicrous"}]}}}),
        Arc::new(TestRegistry::new(fixture(&[OPUS]))),
        "session-1",
        ExtensionMode::Rpc,
        STARTUP,
        Some("settings"),
    )
    .await;
    assert_eq!(selectors(&harness.session_models()), ["anthropic/"]);
    assert!(harness.session_thinking_levels().is_empty());
}

#[tokio::test]
async fn the_session_only_setter_is_used_and_the_durable_thinking_level_is_untouched() {
    let harness = start(
        json!({"model_profile": "daily-heavy"}),
        Arc::new(TestRegistry::new(fixture(&[SUBSCRIPTION_OPUS]))),
        "session-1",
        ExtensionMode::Rpc,
        STARTUP,
        Some("settings"),
    )
    .await;
    assert_eq!(harness.session_thinking_levels(), [ModelThinkingLevel::Xhigh]);
    assert!(harness.durable_thinking_levels().is_empty());
    assert!(!harness.agent_dir.join("settings.json").exists());
}

#[tokio::test]
async fn the_default_logger_records_skipped_candidates_without_raw_error_text() {
    let harness = start(
        json!({}),
        Arc::new(TestRegistry::new(fixture(&[OPUS, GLM])).dead_provider("anthropic").broken_model("zai", "glm-5.3")),
        "session-1",
        ExtensionMode::Rpc,
        STARTUP,
        Some("settings"),
    )
    .await;
    let lines = harness.logger.lines();
    assert!(lines.iter().any(|line| line.starts_with("warn:") && line.contains("skipped anthropic/") && line.contains("refresh")));
    assert!(!lines.iter().any(|line| line.contains(PRIVATE_MARKER)));
    assert!(!format!("{:?}", harness.messages()).contains(PRIVATE_MARKER));
}

#[tokio::test]
async fn a_provider_that_only_fails_on_its_model_headers_keeps_its_sibling_eligible() {
    let harness = start(
        json!({"model_profile": "pair", "model_profiles": {"pair": {"models": ["anthropic/", "zai/glm-5.3"]}}}),
        Arc::new(TestRegistry::new(fixture(&[OPUS, GLM])).broken_model("anthropic", "")),
        "session-1",
        ExtensionMode::Rpc,
        STARTUP,
        Some("settings"),
    )
    .await;
    assert_eq!(selectors(&harness.session_models()), ["zai/glm-5.3"]);
}

#[tokio::test]
async fn a_pool_rotates_onto_its_healthy_slot_and_applies_the_provider() {
    let harness = start(
        json!({}),
        Arc::new(
            TestRegistry::new(fixture(&[OPUS, GLM]))
                .dead_provider("anthropic")
                .dead_slot("anthropic", "expired")
                .with_accounts("anthropic", vec![support::account("expired", false), support::account("healthy", false)]),
        ),
        "session-1",
        ExtensionMode::Rpc,
        STARTUP,
        Some("settings"),
    )
    .await;
    assert_eq!(selectors(&harness.session_models()), ["anthropic/"]);
    // The flat credential is never probed: the walk rotates expired -> healthy and carries the
    // resolved slot into the model scope.
    assert_eq!(
        harness.registry.calls(),
        ["anthropic#expired", "anthropic#healthy", "anthropic/#healthy"]
    );
    assert_eq!(harness.first_details()["authFailed"], json!(null));
    assert!(!harness.logger.lines().iter().any(|line| line.contains(PRIVATE_MARKER)));
}

#[tokio::test]
async fn a_pinned_pool_honors_the_pin_and_skips_the_provider_when_the_pin_fails() {
    let harness = start(
        json!({}),
        Arc::new(
            TestRegistry::new(fixture(&[OPUS, GLM]))
                .dead_slot("anthropic", "expired")
                .with_accounts("anthropic", vec![support::account("healthy", false), support::account("expired", true)]),
        ),
        "session-1",
        ExtensionMode::Rpc,
        STARTUP,
        Some("settings"),
    )
    .await;
    // The pin is the ONLY slot rotation may use, so the healthy sibling and the flat credential are
    // both ignored.
    assert_eq!(selectors(&harness.session_models()), ["zai/glm-5.3"]);
    assert_eq!(harness.registry.calls(), ["anthropic#expired", "zai", "zai/glm-5.3"]);
    assert_eq!(
        harness.first_details()["authFailed"],
        json!([{"provider": "anthropic", "model": "", "reason": "refresh"}])
    );
    assert!(!harness.logger.lines().iter().any(|line| line.contains(PRIVATE_MARKER)));
}

#[tokio::test]
async fn a_registry_reporting_the_runtime_source_turns_rotation_off_so_the_hook_probes_only_the_flat_credential() {
    let harness = start(
        json!({}),
        Arc::new(
            TestRegistry::new(fixture(&[SUBSCRIPTION_OPUS, GLM]))
                .dead_provider("anthropic-subscription")
                .with_accounts("anthropic-subscription", vec![support::account("expired", false), support::account("healthy", false)])
                .with_auth_status(
                    "anthropic-subscription",
                    ProviderAuthStatus { configured: true, source: Some("runtime".into()), label: None },
                ),
        ),
        "session-1",
        ExtensionMode::Rpc,
        STARTUP,
        Some("settings"),
    )
    .await;
    // The registry reports `source: "runtime"`, which turns rotation off, so the pool's healthy
    // sibling is NOT probed and the flat credential decides. The host must map a REAL runtime API key
    // onto this source; this fixture proves only that the hook honors the port.
    assert_eq!(selectors(&harness.session_models()), ["zai/glm-5.3"]);
    assert_eq!(harness.registry.calls(), ["anthropic-subscription", "zai", "zai/glm-5.3"]);
}

#[tokio::test]
async fn the_applied_notice_is_the_only_message_sent() {
    let harness = start(
        json!({"model_profile": "daily-heavy"}),
        Arc::new(TestRegistry::new(fixture(&[SUBSCRIPTION_OPUS]))),
        "session-1",
        ExtensionMode::Rpc,
        STARTUP,
        Some("settings"),
    )
    .await;
    let messages: Vec<&CustomMessage> = harness.messages().iter().collect();
    assert_eq!(messages.len(), 1);
    assert!(messages[0].display);
    assert_eq!(messages[0].custom_type, MODEL_PROFILE_APPLIED_TYPE);
}

#[tokio::test]
async fn a_second_registration_of_the_same_component_still_applies_to_a_fresh_session() {
    let harness = support::harness_in(
        component(json!({"model_profile": "daily-heavy"})),
        Arc::new(TestRegistry::new(fixture(&[SUBSCRIPTION_OPUS]))),
        "session-1",
        ExtensionMode::Rpc,
        "/project",
        tempfile::tempdir().expect("temp dir"),
    );
    harness.start(STARTUP, Some("settings")).await;
    let reloaded = support::harness_in(
        component(json!({"model_profile": "daily-heavy"})),
        Arc::new(TestRegistry::new(fixture(&[SUBSCRIPTION_OPUS]))),
        "session-1",
        ExtensionMode::Rpc,
        "/project",
        tempfile::tempdir().expect("temp dir"),
    );
    reloaded.start(STARTUP, Some("settings")).await;
    assert_eq!(reloaded.session_models().len(), 1);
}

#[tokio::test]
async fn a_json_mode_session_applies_the_profile_like_any_headless_run() {
    let harness = start(
        json!({"model_profile": "daily-heavy"}),
        Arc::new(TestRegistry::new(fixture(&[SUBSCRIPTION_OPUS]))),
        "session-json",
        ExtensionMode::Json,
        STARTUP,
        Some("settings"),
    )
    .await;
    assert_eq!(selectors(&harness.session_models()), ["anthropic-subscription/"]);
}

#[tokio::test]
async fn an_app_server_mode_session_applies_the_profile() {
    let harness = start(
        json!({"model_profile": "daily-heavy"}),
        Arc::new(TestRegistry::new(fixture(&[SUBSCRIPTION_OPUS]))),
        "session-app",
        ExtensionMode::AppServer,
        STARTUP,
        Some("settings"),
    )
    .await;
    assert_eq!(selectors(&harness.session_models()), ["anthropic-subscription/"]);
}

#[tokio::test]
async fn a_provider_removed_by_a_failure_never_returns_on_a_later_rung() {
    let harness = start(
        json!({"model_profile": "pair", "model_profiles": {"pair": {"models": ["anthropic/", "anthropic-subscription/", "zai/glm-5.3"]}}}),
        Arc::new(
            TestRegistry::new(fixture(&[OPUS, SUBSCRIPTION_OPUS, GLM])).dead_provider("anthropic"),
        ),
        "session-1",
        ExtensionMode::Rpc,
        STARTUP,
        Some("settings"),
    )
    .await;
    assert_eq!(selectors(&harness.session_models()), ["anthropic-subscription/"]);
}

#[tokio::test]
async fn the_registry_is_never_probed_when_the_resolution_is_unavailable() {
    let harness = start(
        json!({"model_profile": "daily-normal"}),
        Arc::new(TestRegistry::new(fixture(&[UNRELATED]))),
        "session-1",
        ExtensionMode::Rpc,
        STARTUP,
        Some("settings"),
    )
    .await;
    assert!(harness.registry.calls().is_empty());
}
