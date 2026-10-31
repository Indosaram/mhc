use maho_ai::types::ThinkingLevel;
use maho_ext_recommended_models::*;
#[test]
// Upstream defaults: packages/coding-agent/src/core/extensions/builtin/recommended-models/index.ts:25-30.
fn shipped_order() {
    let index = RECOMMENDED_DEFAULT_MODELS
        .iter()
        .position(|(id, _)| *id == "gpt-6-astra")
        .expect("astra");
    assert_eq!(RECOMMENDED_DEFAULT_MODELS[index].1, ThinkingLevel::Xhigh);
    assert_eq!(
        RECOMMENDED_DEFAULT_MODELS[index + 1],
        ("gpt-6.1-sol", ThinkingLevel::Medium)
    );
    assert_eq!(
        RECOMMENDED_DEFAULT_MODELS[index + 2],
        ("glm-5.3", ThinkingLevel::Max)
    );
}
#[test]
fn aliases() {
    for id in ["kimi-k3-ultrafast", "k3", "K3-FAST-256k-unlocked"] {
        assert_eq!(canonical_model_id(id), "kimi-k3");
    }
    assert_eq!(canonical_model_id("k3.1-pro"), "kimi-k3.1-pro");
    assert_eq!(canonical_model_id("kimi-k3.1-pro"), "kimi-k3.1-pro");
    assert_eq!(canonical_model_id("gpt-6-astra-fast"), "gpt-6-astra");
}
#[test]
// Unknown configured ids use medium: recommended-models/index.ts:51-55.
fn configured_priority() {
    let ids = vec!["glm-5.2".into(), "custom-fast".into()];
    let recommendations = recommendations_for(Some(&ids));
    assert_eq!(recommendations[0].model_id, "glm-5.2");
    assert_eq!(recommendations[0].thinking_level, ThinkingLevel::Medium);
    assert_eq!(recommendations[1].thinking_level, ThinkingLevel::Medium);
}
#[test]
fn explicit_defaults_respected() {
    for provenance in ["settings", "cli", "scoped"] {
        assert!(!can_auto_switch(Some(provenance)));
    }
}
#[test]
fn implicit_defaults_switch() {
    for provenance in ["provider-default", "first-available"] {
        assert!(can_auto_switch(Some(provenance)));
    }
}
#[test]
fn empty_override() {
    assert!(recommendations_for(Some(&[])).is_empty());
    assert!(!recommendations_for(None).is_empty());
}
#[test]
fn lifecycle_registered() {
    use maho_ext_api::*;
    let mut api = ExtensionApi::new(
        LoadedExtension::new("recommended", "/tmp".into(), SourceInfo::default()),
        ExtensionSessionProfile::default(),
        EventBus::default(),
        ExtensionRuntime::default(),
    );
    RecommendedModels.register(&mut api);
    assert_eq!(
        api.get_flag("no-recommended-models"),
        Some(FlagValue::Boolean(false))
    );
    assert_eq!(api.registered.handlers[&EventKind::SessionStart].len(), 1);
    assert_eq!(api.registered.handlers[&EventKind::ModelSelect].len(), 1);
}

fn model(id: &str, provider: &str) -> maho_ai::model::Model {
    maho_ai::model::Model {
        deprecated: None,
        id: id.into(),
        name: id.into(),
        api: "faux".into(),
        provider: provider.into(),
        base_url: "http://unused.invalid".into(),
        reasoning: true,
        thinking_level_map: None,
        input: vec![],
        cost: Default::default(),
        context_window: 1000,
        max_tokens: 100,
        sampling_params: None,
        headers: None,
        cache_retention: None,
        prompt_cache: None,
        supports_assistant_prefill: None,
        upstream_model_id: None,
        service_tier: None,
        recover_text_tool_calls: None,
        compat: None,
        default_thinking_level: None,
        input_limits: None,
    }
}
fn pick(
    available: &[maho_ai::model::Model],
    configured: Option<&[String]>,
) -> Option<(String, String, ThinkingLevel)> {
    let recommendations = recommendations_for(configured);
    find_available_recommendation(&recommendations, available, |_| true).map(
        |(recommendation, model)| {
            (
                model.id.clone(),
                model.provider.clone(),
                recommendation.thinking_level,
            )
        },
    )
}

#[test]
fn shipped_list_order_levels_and_no_gateway_providers() {
    assert_eq!(
        RECOMMENDED_DEFAULT_MODELS,
        [
            ("claude-opus-5-5", ThinkingLevel::Medium),
            ("claude-fable-5-1", ThinkingLevel::Xhigh),
            ("kimi-k3.1-pro", ThinkingLevel::Max),
            ("gpt-6-astra", ThinkingLevel::Xhigh),
            ("gpt-6.1-sol", ThinkingLevel::Medium),
            ("glm-5.3", ThinkingLevel::Max)
        ]
    );
    for (id, _) in RECOMMENDED_DEFAULT_MODELS {
        assert!(!ranked_providers(id).contains(&"opengateway"));
        assert!(!ranked_providers(id).contains(&"openrouter"));
    }
}

#[test]
fn subscription_lane_wins_over_api_listed_first() {
    let available = [
        model("claude-opus-5-5", "anthropic"),
        model("claude-opus-5-5", "anthropic-subscription"),
    ];
    assert_eq!(
        pick(&available, None),
        Some((
            "claude-opus-5-5".into(),
            "anthropic-subscription".into(),
            ThinkingLevel::Medium
        ))
    );
}

#[test]
fn gateway_aggregators_are_skipped_for_kimi_k31_pro() {
    let available = [
        model("anthropic/claude-opus-5-5", "opengateway"),
        model("anthropic/claude-fable-5-1", "openrouter"),
        model("k3.1-pro", "kimi-coding"),
    ];
    assert_eq!(
        pick(&available, None),
        Some(("k3.1-pro".into(), "kimi-coding".into(), ThinkingLevel::Max))
    );
}

#[test]
fn unranked_provider_rung_is_skipped() {
    let available = [
        model("claude-opus-5-5", "venice"),
        model("k3.1-pro", "kimi-coding"),
    ];
    assert_eq!(
        pick(&available, None).map(|(id, _, _)| id),
        Some("k3.1-pro".into())
    );
}

#[test]
fn override_outside_the_table_is_served_by_any_provider() {
    let configured = vec!["custom-model".to_owned()];
    assert_eq!(
        pick(&[model("custom-model", "venice")], Some(&configured)),
        Some((
            "custom-model".into(),
            "venice".into(),
            ThinkingLevel::Medium
        ))
    );
}

#[test]
fn gpt61_sol_beats_older_sol_releases() {
    let available = [
        model("gpt-5.6-sol", "chatgpt-subscription"),
        model("gpt-6-sol", "chatgpt-subscription"),
        model("gpt-6.1-sol", "chatgpt-subscription"),
    ];
    assert_eq!(
        pick(&available, None),
        Some((
            "gpt-6.1-sol".into(),
            "chatgpt-subscription".into(),
            ThinkingLevel::Medium
        ))
    );
}

#[test]
fn astra_ranks_above_sol_at_xhigh() {
    let available = [
        model("gpt-6.1-sol", "chatgpt-subscription"),
        model("gpt-6-astra", "chatgpt-subscription"),
    ];
    assert_eq!(
        pick(&available, None),
        Some((
            "gpt-6-astra".into(),
            "chatgpt-subscription".into(),
            ThinkingLevel::Xhigh
        ))
    );
}

#[test]
fn override_priority_keeps_the_table_level() {
    let configured = vec!["glm-5.3".to_owned()];
    let available = [
        model("k3.1-pro", "kimi-coding"),
        model("glm-5.3", "zai-coding-plan"),
    ];
    assert_eq!(
        pick(&available, Some(&configured)),
        Some((
            "glm-5.3".into(),
            "zai-coding-plan".into(),
            ThinkingLevel::Max
        ))
    );
}

#[test]
fn suffix_and_k31_pro_aliases_count_as_recommended() {
    let recommendations = recommendations_for(None);
    for id in [
        "gpt-6-astra-fast",
        "gpt-6.1-sol-fast",
        "kimi-k3.1-pro",
        "k3.1-pro",
    ] {
        assert!(
            is_recommended(Some(&model(id, "faux")), &recommendations),
            "{id}"
        );
    }
    assert!(!is_recommended(
        Some(&model("off-list", "faux")),
        &recommendations
    ));
}

#[test]
fn k31_pro_is_selected_over_k3_and_k3_is_no_longer_recommended() {
    let available = [model("k3", "kimi-coding"), model("k3.1-pro", "kimi-coding")];
    assert_eq!(
        pick(&available, None),
        Some(("k3.1-pro".into(), "kimi-coding".into(), ThinkingLevel::Max))
    );
    assert!(!is_recommended(
        Some(&model("k3", "kimi-coding")),
        &recommendations_for(None)
    ));
    assert!(
        RECOMMENDED_DEFAULT_MODELS
            .iter()
            .all(|(id, _)| *id != "kimi-k3")
    );
}

#[test]
fn override_naming_k3_still_canonicalizes_to_kimi_k3() {
    let configured = vec!["k3".to_owned()];
    assert_eq!(
        pick(&[model("k3", "kimi-coding")], Some(&configured)),
        Some(("k3".into(), "kimi-coding".into(), ThinkingLevel::Medium))
    );
}

#[test]
fn no_available_recommendation_returns_none() {
    assert_eq!(pick(&[], None), None);
}
