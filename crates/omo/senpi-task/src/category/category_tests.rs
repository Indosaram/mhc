use pretty_assertions::assert_eq;
use serde_json::{Value, json};

use super::*;
use crate::delegate_adapter::DelegateFallbackEntry;
use crate::host::SenpiModelRegistry;
use crate::host::fake::{failing_registry, fixed_registry, model, registry};
use crate::state::{ResolvedModelRecord, ResolvedModelSource};

fn resolve(
    category: &str,
    config: Value,
    models: &dyn SenpiModelRegistry,
) -> CategoryResolutionResult {
    resolve_category(
        category,
        &config,
        models,
        &ResolveCategoryOptions::default(),
    )
    .expect("resolve")
}

struct Resolved {
    spec: ResolvedChildSpec,
    description: Option<String>,
    selection: CategoryModelSelection,
    available_categories: Vec<String>,
}

fn expect_resolved(result: CategoryResolutionResult) -> Resolved {
    match result {
        CategoryResolutionResult::Resolved {
            spec,
            description,
            model_selection,
            available_categories,
            ..
        } => Resolved {
            spec: *spec,
            description,
            selection: *model_selection,
            available_categories,
        },
        other => panic!("Expected resolved category, got {}", other.kind()),
    }
}

fn expect_unavailable(result: CategoryResolutionResult) -> ModelUnavailable {
    match result {
        CategoryResolutionResult::ModelUnavailable(unavailable) => *unavailable,
        other => panic!("Expected model_unavailable, got {}", other.kind()),
    }
}

fn entry(providers: &[&str], model: &str, variant: Option<&str>) -> DelegateFallbackEntry {
    crate::delegate_adapter::fallback_entry(providers, model, variant)
}

fn chain(name: &str) -> Vec<DelegateFallbackEntry> {
    category_fallback_chain(name).expect("chain").to_vec()
}

#[test]
fn resolve_user_overlay_wins_and_prompt_is_appended() {
    let models = registry(vec![model("anthropic", "claude-opus-4-7")]);
    let config = json!({ "categories": { "ultrabrain": {
        "model": "anthropic/claude-opus-4-7", "variant": "max", "prompt_append": "fixture-overlay"
    } } });
    let resolved = expect_resolved(resolve("ultrabrain", config, &models));
    assert_eq!(resolved.spec.provider, "anthropic");
    assert_eq!(resolved.spec.model_id, "claude-opus-4-7");
    assert_eq!(resolved.spec.variant.as_deref(), Some("max"));
    let prompt_append = resolved.spec.prompt_append.expect("prompt append");
    assert_ne!(prompt_append, "fixture-overlay");
    assert!(prompt_append.ends_with("\n\nfixture-overlay"));
}

#[test]
fn resolve_disabled_overlay_explains_reason() {
    let models = registry(vec![model("openai", "gpt-5.5")]);
    let result = resolve(
        "ultrabrain",
        json!({ "categories": { "ultrabrain": { "disable": true } } }),
        &models,
    );
    let CategoryResolutionResult::Disabled {
        reason,
        available_categories,
        ..
    } = result
    else {
        panic!("Expected disabled result, got {}", result.kind());
    };
    assert!(reason.contains("disabled"));
    assert!(available_categories.contains(&"ultrabrain".to_string()));
}

#[test]
fn resolve_omo_fallback_reaches_registry_model() {
    let models = registry(vec![model("google", "gemini-3.1-pro")]);
    let config = json!({ "categories": { "ultrabrain": { "fallback_models": ["google/gemini-3.1-pro high"] } } });
    let resolved = expect_resolved(resolve("ultrabrain", config, &models));
    assert_eq!(resolved.spec.provider, "google");
    assert_eq!(resolved.spec.model_id, "gemini-3.1-pro");
    assert_eq!(resolved.spec.variant.as_deref(), Some("high"));
    assert!(resolved.selection.matched_fallback);
}

#[test]
fn resolve_runtime_fallback_chain_is_retained() {
    let models = registry(vec![
        model("vendor-b", "fallback-model"),
        model("vendor-c", "final-model"),
    ]);
    let config = json!({ "categories": { "quick": {
        "model": "vendor-a/primary-model",
        "fallback_models": ["vendor-b/fallback-model", "vendor-c/final-model"]
    } } });
    let resolved = expect_resolved(resolve("quick", config, &models));
    assert_eq!(resolved.spec.provider, "vendor-b");
    assert_eq!(resolved.spec.model_id, "fallback-model");
    assert_eq!(
        (resolved.spec.requested_model, resolved.spec.fallback_models),
        (
            Some(ResolvedModelRecord {
                variant: Some("low".into()),
                ..ResolvedModelRecord::new(
                    ResolvedModelSource::Category,
                    "vendor-a",
                    "primary-model"
                )
            }),
            Some(vec![ResolvedModelRecord {
                variant: Some("low".into()),
                ..ResolvedModelRecord::new(ResolvedModelSource::Category, "vendor-c", "final-model")
            }]),
        )
    );
}

#[test]
fn resolve_canonical_models_chain_carries_reasoning() {
    let models = registry(vec![
        model("vendor-a", "primary-model"),
        model("vendor-b", "fallback-model"),
    ]);
    let config = json!({ "categories": { "quick": { "models": [
        { "model": "vendor-a/primary-model", "reasoning": "high" },
        { "model": "vendor-b/fallback-model", "reasoning": "off" }
    ] } } });
    let resolved = expect_resolved(resolve("quick", config, &models));
    assert_eq!(resolved.spec.provider, "vendor-a");
    assert_eq!(resolved.spec.model_id, "primary-model");
    assert_eq!(resolved.spec.reasoning_effort.as_deref(), Some("high"));
    assert_eq!(
        resolved.spec.fallback_models.expect("fallbacks")[0].model_id,
        "fallback-model"
    );
}

#[test]
fn resolve_canonical_models_win_over_legacy_fallbacks() {
    let models = registry(vec![model("vendor-a", "primary-model")]);
    let config = json!({ "categories": { "quick": {
        "models": [{ "model": "vendor-a/primary-model", "reasoning": "high" }],
        "model": "vendor-z/legacy-primary",
        "fallback_models": ["vendor-z/legacy-fallback"]
    } } });
    let resolved = expect_resolved(resolve("quick", config, &models));
    assert_eq!(resolved.spec.provider, "vendor-a");
    assert_eq!(resolved.spec.model_id, "primary-model");
}

#[test]
fn resolve_simple_category_keeps_canonical_reasoning() {
    let models = registry(vec![model("vendor-a", "solo-model")]);
    let config = json!({ "categories": { "deep-low": { "model": "vendor-a/solo-model", "reasoning": "medium" } } });
    let resolved = expect_resolved(resolve("deep-low", config, &models));
    assert_eq!(resolved.spec.model_id, "solo-model");
    assert_eq!(resolved.spec.reasoning_effort.as_deref(), Some("medium"));
}

#[test]
fn resolve_quick_chain_reaches_luna_fast() {
    let models = registry(vec![model("openai", "gpt-6-luna-fast")]);
    let resolved = expect_resolved(resolve("quick", json!({}), &models));
    assert_eq!(resolved.spec.provider, "openai");
    assert_eq!(resolved.spec.model_id, "gpt-6-luna-fast");
    assert_eq!(resolved.spec.variant.as_deref(), Some("low"));
    assert!(resolved.selection.matched_fallback);
    assert_eq!(
        resolved.selection.fallback_entry,
        Some(entry(
            &["chatgpt-subscription", "openai"],
            "gpt-6-luna-fast",
            Some("low")
        ))
    );
}

#[test]
fn writing_prefers_argon_then_opus_with_accepted_effort() {
    for (provider, id, effort) in [
        ("google", "gemini-4-argon", "low"),
        ("anthropic", "claude-opus-5-5", "low"),
        ("anthropic", "claude-opus-4-6", "max"),
    ] {
        let resolved = expect_resolved(resolve(
            "writing",
            json!({}),
            &registry(vec![model(provider, id)]),
        ));
        assert_eq!(
            (
                resolved.spec.provider.as_str(),
                resolved.spec.model_id.as_str(),
                resolved.spec.variant.as_deref()
            ),
            (provider, id, Some(effort))
        );
    }
}

#[test]
fn exact_gpt_lanes_prefer_subscription_but_not_its_fast_sku() {
    for (category, id, effort) in [
        ("deep-low", "gpt-6.2-sol", "medium"),
        ("deep-high", "gpt-6.2-astra", "high"),
        ("ultrabrain", "gpt-6.2-astra", "max"),
    ] {
        for (subscription_id, winner) in [
            (id.to_string(), "chatgpt-subscription"),
            (format!("{id}-fast"), "openai"),
        ] {
            let host = registry(vec![
                model("openai", id),
                model("chatgpt-subscription", &subscription_id),
            ]);
            let got = expect_resolved(resolve(category, json!({}), &host));
            assert_eq!(
                (
                    got.spec.provider.as_str(),
                    got.spec.model_id.as_str(),
                    got.spec.variant.as_deref()
                ),
                (winner, id, Some(effort))
            );
            assert!(got.spec.fallback_models.is_none());
        }
        for unavailable in [
            model("github-copilot", id),
            model("openai", &format!("{id}-fast")),
            model("chatgpt-subscription", "gpt-6.1-sol"),
        ] {
            let got =
                expect_unavailable(resolve(category, json!({}), &registry(vec![unavailable])));
            assert!(!contains(&got.available_categories, category));
        }
    }
}

#[test]
fn resolve_reaches_system_default() {
    let models = registry(vec![model("local", "system-default")]);
    let options = ResolveCategoryOptions {
        system_default_model: Some("local/system-default".into()),
    };
    let resolved =
        expect_resolved(resolve_category("quick", &json!({}), &models, &options).expect("resolve"));
    assert_eq!(resolved.spec.provider, "local");
    assert_eq!(resolved.spec.model_id, "system-default");
    assert!(!resolved.selection.matched_fallback);
}

#[test]
fn resolve_absent_selected_model_names_attempted_and_available() {
    let models = registry(vec![model("anthropic", "claude-sonnet-4-6")]);
    let config = json!({ "categories": { "quick": { "model": "openai/not-installed" } } });
    let unavailable = expect_unavailable(resolve("quick", config, &models));
    assert_eq!(unavailable.category, "quick");
    assert_eq!(
        unavailable.attempted_model.as_deref(),
        Some("openai/not-installed")
    );
    assert_eq!(
        unavailable.available_models,
        vec!["anthropic/claude-sonnet-4-6".to_string()]
    );
    assert_eq!(unavailable.nearest_fallback, None);
}

#[test]
fn resolve_overlay_generation_params_reach_child_spec() {
    let models = registry(vec![model("chatgpt-subscription", "gpt-6-luna-fast")]);
    let config = json!({ "categories": { "quick": {
        "temperature": 0.3, "top_p": 0.8, "maxTokens": 4096,
        "thinking": { "type": "enabled", "budgetTokens": 1024 },
        "reasoningEffort": "medium",
        "tools": { "read": true, "write": false },
        "prompt_append": "fixture-quick-overlay"
    } } });
    let spec = expect_resolved(resolve("quick", config, &models)).spec;
    assert_eq!(spec.temperature, Some(0.3));
    assert_eq!(spec.top_p, Some(0.8));
    assert_eq!(spec.max_tokens, Some(4096));
    assert_eq!(
        spec.thinking,
        Some(json!({ "type": "enabled", "budgetTokens": 1024 }))
    );
    assert_eq!(spec.reasoning_effort.as_deref(), Some("medium"));
    assert_eq!(spec.tools, Some(json!({ "read": true, "write": false })));
    let prompt_append = spec.prompt_append.expect("prompt append");
    assert_ne!(prompt_append, "fixture-quick-overlay");
    assert!(prompt_append.ends_with("\n\nfixture-quick-overlay"));
}

#[test]
fn resolve_preserves_custom_description() {
    let models = registry(vec![model("openai", "custom-model")]);
    let config = json!({ "categories": { "custom-review": {
        "model": "openai/custom-model", "description": "Custom review lane"
    } } });
    let resolved = expect_resolved(resolve("custom-review", config, &models));
    assert_eq!(resolved.description.as_deref(), Some("Custom review lane"));
}

#[test]
fn builtin_defaults_pin_machine_routing_fields() {
    let defaults: Vec<(&str, &str, Option<&str>)> = BUILTIN_CATEGORY_DEFAULTS
        .iter()
        .map(|definition| {
            (
                definition.name,
                definition.config.model,
                definition.config.variant,
            )
        })
        .collect();
    assert_eq!(
        defaults,
        vec![
            ("visual-engineering", "google/gemini-4-argon", Some("low")),
            ("artistry", "anthropic/claude-fable-5-2", Some("max")),
            (
                "ultrabrain",
                "chatgpt-subscription/gpt-6.2-astra",
                Some("max")
            ),
            (
                "deep-low",
                "chatgpt-subscription/gpt-6.2-sol",
                Some("medium")
            ),
            (
                "deep-high",
                "chatgpt-subscription/gpt-6.2-astra",
                Some("high")
            ),
            ("quick", "chatgpt-subscription/gpt-6-luna-fast", Some("low")),
            (
                "unspecified-low",
                "anthropic/claude-sonnet-5-5",
                Some("medium")
            ),
            (
                "unspecified-high",
                "anthropic/claude-opus-5-5",
                Some("medium")
            ),
            ("architect", "anthropic/claude-fable-5-2", Some("xhigh")),
            ("writing", "google/gemini-4-argon", Some("low")),
        ]
    );
}

// ---- category/available-categories.test.ts ----

#[test]
fn available_exposes_architect_with_either_exact_fable_version() {
    for (provider, id) in [
        ("anthropic", "claude-fable-5-2"),
        ("anthropic", "claude-fable-5-1"),
        ("github-copilot", "claude-fable-5.1"),
    ] {
        let host = registry(vec![model(provider, id)]);
        let got = expect_resolved(resolve("architect", json!({}), &host));
        assert_eq!(got.spec.model_id, id);
        assert!(contains(&got.available_categories, "architect"));
    }
}

#[test]
fn available_hides_architect_without_gate_model() {
    let names = resolve_available_category_names(
        &json!({}),
        &registry(vec![model("omo-mock", "mock-weak")]),
    );
    assert_eq!(names, Vec::<String>::new());
}

#[test]
fn available_explicit_declaration_exposes_architect() {
    let config =
        json!({ "categories": { "architect": { "model": "anthropic/claude-fable-5-2" } } });
    let names =
        resolve_available_category_names(&config, &registry(vec![model("omo-mock", "mock-weak")]));
    assert!(names.contains(&"architect".to_string()));
}

#[test]
fn available_throwing_registry_degrades_to_ungated_list() {
    let names =
        resolve_available_category_names(&json!({}), &failing_registry("registry not ready"));
    assert!(names.contains(&"architect".to_string()));
}

// ---- category/anthropic-categories.test.ts ----

fn architect() -> &'static BuiltinCategoryDefinition {
    builtin_category("architect").expect("architect")
}

#[test]
fn anthropic_architect_description_names_fable() {
    assert!(architect().description.contains("Fable 5"));
}

#[test]
fn anthropic_architect_description_names_sensitive_content() {
    assert!(
        architect()
            .description
            .contains("security- and biology-related content")
    );
}

#[test]
fn anthropic_architect_description_prescribes_refusal_recovery() {
    assert!(
        architect()
            .description
            .contains("indirectly-phrased sub-questions")
    );
    assert!(architect().description.contains("reasoning yourself"));
}

#[test]
fn anthropic_architect_requires_fable_at_xhigh() {
    assert_eq!(
        (architect().config, architect().requires_model),
        (
            BuiltinCategoryConfig {
                model: "anthropic/claude-fable-5-2",
                variant: Some("xhigh")
            },
            &["claude-fable-5-2", "claude-fable-5-1"][..]
        )
    );
}

// ---- category/category-routing-policy.test.ts ----

#[test]
fn unspecified_low_keeps_sol_versions_in_order_and_copilot_on_legacy_sol() {
    for (provider, models, winner) in [
        (
            "openai",
            vec!["gpt-5.6-sol", "gpt-6.1-sol", "gpt-6.2-sol"],
            "gpt-6.2-sol",
        ),
        ("openai", vec!["gpt-5.6-sol", "gpt-6.1-sol"], "gpt-6.1-sol"),
        (
            "github-copilot",
            vec!["gpt-5.6-sol", "gpt-6.2-sol"],
            "gpt-5.6-sol",
        ),
        ("zai", vec!["glm-5.3"], "glm-5.3"),
    ] {
        let host = registry(models.into_iter().map(|id| model(provider, id)).collect());
        let got = expect_resolved(resolve("unspecified-low", json!({}), &host));
        assert_eq!(got.spec.model_id, winner);
        assert_eq!(
            got.spec.variant.as_deref(),
            Some(if provider == "zai" { "high" } else { "medium" })
        );
    }
}

// ---- category/dead-chain.test.ts ----

fn opus_only() -> crate::host::fake::FakeRegistry {
    registry(vec![
        model("omo-mock", "mock-parent"),
        model("anthropic", "claude-opus-5-5"),
    ])
}

fn contains(names: &[String], name: &str) -> bool {
    names.iter().any(|candidate| candidate == name)
}

#[test]
fn dead_chain_fails_with_attempted_chain_and_missing_providers() {
    let unavailable = expect_unavailable(resolve("quick", json!({}), &opus_only()));
    assert_eq!(unavailable.attempted_chain, Some(chain("quick")));
    assert_eq!(
        unavailable.missing_providers,
        Some(
            [
                "chatgpt-subscription",
                "openai",
                "anthropic-subscription",
                "anthropic-api",
                "github-copilot",
                "deepseek",
                "qwen-token-plan",
                "alibaba-token-plan",
                "bailian-coding-plan",
                "opencode-go",
                "xai",
                "zai",
                "zai-coding-cn",
                "xiaomi",
            ]
            .map(String::from)
            .to_vec()
        )
    );
}

#[test]
fn dead_chain_builtin_is_excluded_from_gated_list() {
    let result = resolve("quick", json!({}), &opus_only());
    assert!(!contains(result.available_categories(), "quick"));
}

#[test]
fn dead_chain_live_builtins_stay_listed() {
    let result = resolve("quick", json!({}), &opus_only());
    assert!(contains(
        result.available_categories(),
        "visual-engineering"
    ));
    assert!(contains(result.available_categories(), "unspecified-high"));
}

#[test]
fn dead_chain_gate_failure_carries_chain_details() {
    let unavailable = expect_unavailable(resolve("architect", json!({}), &opus_only()));
    assert_eq!(unavailable.attempted_chain, Some(chain("architect")));
    let missing = unavailable.missing_providers.expect("missing providers");
    assert!(contains(&missing, "anthropic-api"));
    assert!(!contains(&missing, "anthropic"));
}

#[test]
fn dead_chain_kimi_transform_keeps_category_alive() {
    let result = resolve(
        "unspecified-high",
        json!({}),
        &registry(vec![model("kimi-coding", "k3.1-pro")]),
    );
    assert_eq!(result.kind(), "resolved");
    assert!(contains(result.available_categories(), "unspecified-high"));
    assert!(!contains(result.available_categories(), "quick"));
}

#[test]
fn dead_chain_unlisted_gateway_leaves_category_unavailable() {
    let result = resolve(
        "deep-low",
        json!({}),
        &registry(vec![model("vercel", "openai/gpt-6.2-sol")]),
    );
    assert_eq!(result.kind(), "model_unavailable");
    assert!(!contains(result.available_categories(), "deep-low"));
}

#[test]
fn dead_chain_user_model_is_never_gated() {
    let config = json!({ "categories": { "quick": { "model": "omo-mock/mock-parent" } } });
    let result = resolve(
        "quick",
        config,
        &registry(vec![model("omo-mock", "mock-parent")]),
    );
    assert_eq!(result.kind(), "resolved");
    assert!(contains(result.available_categories(), "quick"));
}

#[test]
fn dead_chain_missing_user_model_is_plain_miss() {
    let config = json!({ "categories": { "quick": { "model": "omo-mock/absent" } } });
    let unavailable = expect_unavailable(resolve(
        "quick",
        config,
        &registry(vec![model("omo-mock", "mock-parent")]),
    ));
    assert_eq!(unavailable.attempted_chain, None);
    assert!(contains(&unavailable.available_categories, "quick"));
}

#[test]
fn dead_chain_disabled_result_returns_gated_list() {
    let result = resolve(
        "quick",
        json!({ "categories": { "quick": { "disable": true } } }),
        &opus_only(),
    );
    assert_eq!(result.kind(), "disabled");
    assert!(contains(result.available_categories(), "quick"));
    assert!(contains(
        result.available_categories(),
        "visual-engineering"
    ));
    assert!(!contains(result.available_categories(), "deep-low"));
}

#[test]
fn dead_chain_unknown_result_returns_gated_list() {
    let result = resolve("nope", json!({}), &opus_only());
    assert_eq!(result.kind(), "not_found");
    assert!(contains(
        result.available_categories(),
        "visual-engineering"
    ));
    assert!(!contains(result.available_categories(), "quick"));
    assert!(!contains(result.available_categories(), "deep-low"));
}

// ---- category/gating.test.ts ----

fn pre_gating_models() -> Vec<Value> {
    vec![
        model("google", "gemini-3.1-pro"),
        model("anthropic", "claude-opus-5"),
        model("opencode-go", "glm-5.2"),
        model("kimi-coding", "k3"),
    ]
}

#[test]
fn gating_architect_unavailable_on_cross_family_models() {
    let unavailable = expect_unavailable(resolve(
        "architect",
        json!({}),
        &registry(pre_gating_models()),
    ));
    assert_eq!(
        unavailable.attempted_model.as_deref(),
        Some("anthropic/claude-fable-5-2")
    );
    assert!(!contains(&unavailable.available_categories, "architect"));
}

#[test]
fn gating_architect_resolves_with_fable() {
    let mut models = pre_gating_models();
    models.push(model("anthropic", "claude-fable-5-2"));
    let resolved = expect_resolved(resolve("architect", json!({}), &registry(models)));
    assert_eq!(
        (
            resolved.spec.provider.as_str(),
            resolved.spec.model_id.as_str(),
            resolved.spec.variant.as_deref()
        ),
        ("anthropic", "claude-fable-5-2", Some("xhigh"))
    );
    assert!(contains(&resolved.available_categories, "architect"));
}

#[test]
fn gating_architect_explicit_entry_bypasses_gate() {
    let config = json!({ "categories": { "architect": { "model": "kimi-coding/k3" } } });
    let resolved = expect_resolved(resolve(
        "architect",
        config,
        &registry(vec![model("kimi-coding", "k3")]),
    ));
    assert_eq!(resolved.spec.model_id, "k3");
}

#[test]
fn gating_architect_description_only_entry_bypasses_gate() {
    let config = json!({ "categories": { "architect": { "description": "House architecture consultant" } } });
    let result = resolve(
        "architect",
        config,
        &registry(vec![model("anthropic", "claude-opus-5")]),
    );
    assert!(contains(result.available_categories(), "architect"));
}

#[test]
fn gating_ultrabrain_unavailable_on_cross_family_models() {
    let unavailable = expect_unavailable(resolve(
        "ultrabrain",
        json!({}),
        &registry(pre_gating_models()),
    ));
    assert_eq!(
        unavailable.attempted_model.as_deref(),
        Some("chatgpt-subscription/gpt-6.2-astra")
    );
    assert!(!contains(&unavailable.available_categories, "ultrabrain"));
}

#[test]
fn gating_ultrabrain_resolves_with_astra_at_max() {
    let resolved = expect_resolved(resolve(
        "ultrabrain",
        json!({}),
        &registry(vec![model("openai", "gpt-6.2-astra")]),
    ));
    assert_eq!(
        (
            resolved.spec.provider.as_str(),
            resolved.spec.model_id.as_str(),
            resolved.spec.variant.as_deref()
        ),
        ("openai", "gpt-6.2-astra", Some("max"))
    );
}

#[test]
fn gating_ultrabrain_zen_astra_satisfies_gate() {
    let resolved = expect_resolved(resolve(
        "ultrabrain",
        json!({}),
        &registry(vec![model("opencode", "gpt-6.2-astra")]),
    ));
    assert_eq!(resolved.spec.provider, "opencode");
    assert_eq!(resolved.spec.variant.as_deref(), Some("max"));
}

#[test]
fn gating_deep_unavailable_on_cross_family_models() {
    let unavailable = expect_unavailable(resolve(
        "deep-low",
        json!({}),
        &registry(pre_gating_models()),
    ));
    assert_eq!(
        unavailable.attempted_model.as_deref(),
        Some("chatgpt-subscription/gpt-6.2-sol")
    );
    assert!(!contains(&unavailable.available_categories, "deep-low"));
}

#[test]
fn gating_deep_resolves_with_sol_at_medium() {
    let resolved = expect_resolved(resolve(
        "deep-low",
        json!({}),
        &registry(vec![model("openai", "gpt-6.2-sol")]),
    ));
    assert_eq!(
        (
            resolved.spec.provider.as_str(),
            resolved.spec.model_id.as_str(),
            resolved.spec.variant.as_deref()
        ),
        ("openai", "gpt-6.2-sol", Some("medium"))
    );
    assert!(contains(&resolved.available_categories, "deep-low"));
}

#[test]
fn gating_deep_explicit_entry_bypasses_gate() {
    let config = json!({ "categories": { "deep-low": { "model": "anthropic/claude-opus-5" } } });
    let resolved = expect_resolved(resolve(
        "deep-low",
        config,
        &registry(vec![model("anthropic", "claude-opus-5")]),
    ));
    assert_eq!(resolved.spec.model_id, "claude-opus-5");
}

#[test]
fn gating_visual_engineering_kimi_is_real_chain_fallback() {
    let resolved = expect_resolved(resolve(
        "visual-engineering",
        json!({}),
        &registry(vec![model("kimi-coding", "k3.1-pro")]),
    ));
    assert!(resolved.selection.matched_fallback);
    assert_eq!(resolved.spec.variant.as_deref(), Some("max"));
}

#[test]
fn gating_artistry_fable_is_primary_hit() {
    let resolved = expect_resolved(resolve(
        "artistry",
        json!({}),
        &registry(vec![model("anthropic", "claude-fable-5-2")]),
    ));
    assert!(!resolved.selection.matched_fallback);
}

#[test]
fn gating_unrelated_vendor_does_not_open_fable_gate() {
    let result = resolve(
        "architect",
        json!({}),
        &registry(vec![model("custom", "unrelated/claude-fable-5-2")]),
    );
    assert_eq!(result.kind(), "model_unavailable");
    assert!(!contains(result.available_categories(), "architect"));
}

#[test]
fn gating_unrelated_vendor_does_not_open_sol_gate() {
    let result = resolve(
        "ultrabrain",
        json!({}),
        &registry(vec![model("custom", "unrelated/gpt-5.6-sol")]),
    );
    assert_eq!(result.kind(), "model_unavailable");
    assert!(!contains(result.available_categories(), "ultrabrain"));
}

#[test]
fn gating_vercel_gateway_does_not_open_gate() {
    let result = resolve(
        "ultrabrain",
        json!({}),
        &registry(vec![model("vercel", "openai/gpt-6.2-astra")]),
    );
    assert_eq!(result.kind(), "model_unavailable");
    assert!(!contains(result.available_categories(), "ultrabrain"));
}

#[test]
fn gating_ungated_category_keeps_chain_fallback() {
    let resolved = expect_resolved(resolve(
        "quick",
        json!({}),
        &registry(vec![model("openai", "gpt-6-luna-fast")]),
    ));
    assert_eq!(resolved.spec.model_id, "gpt-6-luna-fast");
    assert!(contains(&resolved.available_categories, "quick"));
}

#[test]
fn gating_unmet_gates_leave_other_categories_listed() {
    let result = resolve(
        "quick",
        json!({}),
        &registry(vec![model("openai", "gpt-6-luna-fast")]),
    );
    let names = result.available_categories();
    assert!(!contains(names, "architect"));
    assert!(!contains(names, "ultrabrain"));
    assert!(!contains(names, "deep-low"));
    assert!(contains(names, "quick"));
}

// ---- category/resolve-category-boundary.test.ts ----

const QUICK_DEFAULT: &str = "chatgpt-subscription/gpt-6-luna-fast";

fn quick_model() -> Value {
    model("chatgpt-subscription", "gpt-6-luna-fast")
}

fn assert_sanitized(result: CategoryResolutionResult, available: &[&str], hidden: &[&str]) {
    let rendered = format!("{result:?}");
    let unavailable = expect_unavailable(result);
    assert_eq!(unavailable.attempted_model.as_deref(), Some(QUICK_DEFAULT));
    assert_eq!(
        unavailable.available_models,
        available
            .iter()
            .map(|model| (*model).to_string())
            .collect::<Vec<_>>()
    );
    for marker in hidden {
        assert!(
            !rendered.contains(marker),
            "{marker} leaked into {rendered}"
        );
    }
}

#[test]
fn boundary_header_bearing_model_is_accepted() {
    let header_model = json!({
        "provider": "chatgpt-subscription", "id": "gpt-6-luna-fast", "headers": { "User-Agent": "test" }
    });
    let resolved = expect_resolved(resolve(
        "quick",
        json!({}),
        &registry(vec![header_model.clone()]),
    ));
    assert_eq!(resolved.spec.provider, "chatgpt-subscription");
    assert_eq!(resolved.spec.model_id, "gpt-6-luna-fast");
    assert_eq!(resolved.spec.model, header_model);
}

#[test]
fn boundary_malformed_available_entry_is_sanitized() {
    let result = resolve("quick", json!({}), &fixed_registry(json!([null]), None));
    assert_eq!(result.kind(), "model_unavailable");
    assert_sanitized(result, &[], &[]);
}

#[test]
fn boundary_non_string_available_identity_is_sanitized() {
    // A throwing accessor has no JSON equivalent; the nearest boundary input is a provider field
    // that is not a string, which must be dropped the same way without leaking its content.
    let entry = json!({ "provider": { "marker": "hidden available accessor marker" }, "id": "gpt-6-luna-fast" });
    let result = resolve("quick", json!({}), &fixed_registry(json!([entry]), None));
    assert_sanitized(result, &[], &["hidden available accessor marker"]);
}

#[test]
fn boundary_malformed_find_results_are_sanitized() {
    let malformed = [
        json!({}),
        json!({ "provider": { "secret": "hidden" }, "id": ["gpt-6-luna-fast"] }),
        json!({ "provider": "chatgpt-subscription", "id": "gpt-6-luna-fast", "password": "hidden" }),
        json!({ "provider": "chatgpt-subscription", "id": "gpt-6-luna-fast", "accessToken": "hidden" }),
        json!({ "provider": "chatgpt-subscription", "id": "gpt-6-luna-fast", "privateToken": "hidden" }),
    ];
    for find_result in malformed {
        let models = fixed_registry(json!([quick_model()]), Some(find_result));
        assert_sanitized(
            resolve("quick", json!({}), &models),
            &[QUICK_DEFAULT],
            &["hidden"],
        );
    }
}

#[test]
fn boundary_non_string_find_identity_is_sanitized() {
    // Throwing-accessor analogue for `find`: a non-string provider is rejected without leaking.
    let find_result =
        json!({ "provider": ["hidden find accessor marker"], "id": "gpt-6-luna-fast" });
    let models = fixed_registry(json!([quick_model()]), Some(find_result));
    assert_sanitized(
        resolve("quick", json!({}), &models),
        &[QUICK_DEFAULT],
        &["hidden find accessor marker"],
    );
}

#[test]
fn boundary_empty_or_mismatched_find_identity_is_rejected() {
    let malformed = [
        json!({ "provider": "", "id": "" }),
        json!({ "provider": "evil", "id": "other" }),
        json!({ "provider": "chatgpt-subscription", "id": "" }),
    ];
    for find_result in malformed {
        let models = fixed_registry(json!([quick_model()]), Some(find_result));
        assert_sanitized(
            resolve("quick", json!({}), &models),
            &[QUICK_DEFAULT],
            &["evil", "other"],
        );
    }
}

#[test]
fn boundary_inherited_identity_fields_are_rejected() {
    // JSON has no prototype chain; the equivalent untrusted shape nests the identity one level
    // down, where it must not be read as the model's own fields.
    let find_result = json!({ "__proto__": {
        "provider": "chatgpt-subscription", "id": "gpt-6-luna-fast", "privateToken": "hidden"
    } });
    let models = fixed_registry(json!([quick_model()]), Some(find_result));
    assert_sanitized(
        resolve("quick", json!({}), &models),
        &[QUICK_DEFAULT],
        &["hidden"],
    );
}

#[test]
fn boundary_non_array_availability_is_sanitized() {
    let malformed = [
        Value::Null,
        json!({ "0": quick_model(), "length": 1 }),
        json!("chatgpt-subscription/gpt-6-luna-fast"),
    ];
    for available in malformed {
        let models = fixed_registry(available, Some(quick_model()));
        assert_sanitized(resolve("quick", json!({}), &models), &[], &[]);
    }
}

#[test]
fn boundary_prototype_shaped_names_are_not_found() {
    let models = registry(vec![quick_model()]);
    for category in ["__proto__", "toString", "hasOwnProperty"] {
        let result = resolve(category, json!({}), &models);
        assert_eq!(result.kind(), "not_found");
        assert!(contains(result.available_categories(), "quick"));
    }
}

// ---- model-exclusion.test.ts ----

fn fable_and_kimi() -> Vec<Value> {
    vec![
        model("anthropic", "claude-fable-5-1"),
        model("anthropic", "claude-opus-5-5"),
        model("kimi-for-coding", "k3"),
        model("openai", "gpt-5.6-sol"),
    ]
}

#[test]
fn excluded_fable_head_rung_leads_with_the_next_rung() {
    let models = registry(fable_and_kimi());
    let resolved = expect_resolved(resolve(
        "artistry",
        json!({ "exclude_models": ["claude-fable-*"] }),
        &models,
    ));
    assert_eq!(resolved.spec.provider, "anthropic");
    assert_eq!(resolved.spec.model_id, "claude-opus-5-5");
    assert_eq!(resolved.spec.variant.as_deref(), Some("max"));
    assert!(resolved.selection.matched_fallback);
    let fallbacks = resolved.spec.fallback_models.unwrap_or_default();
    assert!(
        fallbacks
            .iter()
            .all(|fallback| fallback.model_id != "claude-fable-5-1"),
        "the excluded Fable rung must never reach the runtime chain: {fallbacks:?}"
    );
}

#[test]
fn excluded_user_pin_is_treated_like_a_model_with_no_login() {
    let models = registry(fable_and_kimi());
    let kept = expect_resolved(resolve(
        "artistry",
        json!({ "categories": { "artistry": { "model": "openai/gpt-5.6-sol" } } }),
        &models,
    ));
    assert_eq!(kept.spec.provider, "openai");
    assert_eq!(kept.spec.model_id, "gpt-5.6-sol");

    let alone = resolve(
        "artistry",
        json!({
            "exclude_models": ["openai/gpt-5.6-sol"],
            "categories": { "artistry": { "model": "openai/gpt-5.6-sol" } }
        }),
        &models,
    );
    assert_eq!(alone.kind(), "model_unavailable");

    let walked = expect_resolved(resolve(
        "artistry",
        json!({
            "exclude_models": ["openai/gpt-5.6-sol"],
            "categories": { "artistry": {
                "model": "openai/gpt-5.6-sol",
                "fallback_models": ["kimi-for-coding/k3"]
            } }
        }),
        &models,
    ));
    assert_eq!(walked.spec.provider, "kimi-for-coding");
    assert_eq!(walked.spec.model_id, "k3");
}

#[test]
fn category_with_every_rung_excluded_is_gone_from_the_listing() {
    let fable_only = registry(vec![
        model("anthropic", "claude-fable-5-1"),
        model("anthropic", "claude-fable-5-2"),
    ]);
    let filtered = resolve_available_category_names(
        &json!({ "exclude_models": ["claude-fable-*"] }),
        &fable_only,
    );
    assert!(!contains(&filtered, "artistry"));
    let unfiltered = resolve_available_category_names(&json!({}), &fable_only);
    assert!(contains(&unfiltered, "artistry"));
}

#[test]
fn not_found_listing_observes_excluded_models() {
    let fable_only = registry(vec![model("anthropic", "claude-fable-5-1")]);
    let filtered = resolve(
        "no-such-category",
        json!({ "exclude_models": ["*"] }),
        &fable_only,
    );
    assert_eq!(filtered.kind(), "not_found");
    assert!(!contains(filtered.available_categories(), "artistry"));
    let unfiltered = resolve("no-such-category", json!({}), &fable_only);
    assert!(contains(unfiltered.available_categories(), "artistry"));
}

#[test]
fn empty_exclude_models_list_leaves_the_listing_alone() {
    let fable_only = registry(vec![model("anthropic", "claude-fable-5-2")]);
    assert_eq!(
        resolve_available_category_names(&json!({ "exclude_models": [] }), &fable_only),
        resolve_available_category_names(&json!({}), &fable_only)
    );
}

#[test]
fn excluded_listing_still_degrades_on_a_malformed_registry() {
    let malformed = fixed_registry(json!({ "not": "an array" }), None);
    let names = resolve_available_category_names(&json!({ "exclude_models": ["*"] }), &malformed);
    assert!(contains(&names, "architect"));
}
