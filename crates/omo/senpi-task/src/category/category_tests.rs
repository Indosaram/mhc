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
    DelegateFallbackEntry::new(providers, model, variant)
}

fn chain(name: &str) -> Vec<DelegateFallbackEntry> {
    category_fallback_chain(name).expect("chain").to_vec()
}

const OPENAI_RUNG: &[&str] = &[
    "openai",
    "quotio-openai",
    "github-copilot",
    "opencode",
    "vercel",
];

// ---- category/resolve-category.test.ts ----

struct Gpt56Case {
    category: &'static str,
    model_id: &'static str,
    native_variant: &'static str,
    mixed_winner: (&'static str, &'static str, &'static str),
    copilot_variant: &'static str,
    copilot_fallback_entry: DelegateFallbackEntry,
}

fn gpt56_category_cases() -> Vec<Gpt56Case> {
    vec![
        Gpt56Case {
            category: "ultrabrain",
            model_id: "gpt-5.6-sol",
            native_variant: "max",
            mixed_winner: ("vercel", "openai/gpt-5.6-sol", "max"),
            copilot_variant: "max",
            copilot_fallback_entry: entry(&["github-copilot"], "gpt-5.6-sol", Some("max")),
        },
        Gpt56Case {
            category: "deep",
            model_id: "gpt-5.6-sol",
            native_variant: "medium",
            mixed_winner: ("github-copilot", "gpt-5.6-sol", "medium"),
            copilot_variant: "medium",
            copilot_fallback_entry: entry(OPENAI_RUNG, "gpt-5.6-sol", Some("medium")),
        },
        Gpt56Case {
            category: "unspecified-low",
            model_id: "gpt-5.6-terra",
            native_variant: "high",
            mixed_winner: ("github-copilot", "gpt-5.6-terra", "high"),
            copilot_variant: "high",
            copilot_fallback_entry: entry(OPENAI_RUNG, "gpt-5.6-terra", Some("high")),
        },
    ]
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
            Some(ResolvedModelRecord::new(
                ResolvedModelSource::Category,
                "vendor-a",
                "primary-model"
            )),
            Some(vec![ResolvedModelRecord::new(
                ResolvedModelSource::Category,
                "vendor-c",
                "final-model"
            )]),
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
    let config = json!({ "categories": { "deep": { "model": "vendor-a/solo-model", "reasoning": "medium" } } });
    let resolved = expect_resolved(resolve("deep", config, &models));
    assert_eq!(resolved.spec.model_id, "solo-model");
    assert_eq!(resolved.spec.reasoning_effort.as_deref(), Some("medium"));
}

#[test]
fn resolve_quick_chain_reaches_luna_fast() {
    let models = registry(vec![model("openai-codex", "gpt-5.6-luna-fast")]);
    let resolved = expect_resolved(resolve("quick", json!({}), &models));
    assert_eq!(resolved.spec.provider, "openai-codex");
    assert_eq!(resolved.spec.model_id, "gpt-5.6-luna-fast");
    assert_eq!(resolved.spec.variant.as_deref(), Some("low"));
    assert!(resolved.selection.matched_fallback);
    assert_eq!(
        resolved.selection.fallback_entry,
        Some(entry(&["openai-codex"], "gpt-5.6-luna-fast", Some("low")))
    );
}

#[test]
fn resolve_writing_default_selects_kimi_k3() {
    let models = registry(vec![model("kimi-coding", "k3")]);
    let resolved = expect_resolved(resolve("writing", json!({}), &models));
    assert_eq!(resolved.spec.provider, "kimi-coding");
    assert_eq!(resolved.spec.model_id, "k3");
    assert_eq!(resolved.spec.variant.as_deref(), Some("low"));
    assert!(!resolved.selection.matched_fallback);
}

#[test]
fn resolve_writing_falls_back_to_k3_rung() {
    let models = registry(vec![model("opencode-go", "kimi-k3")]);
    let resolved = expect_resolved(resolve("writing", json!({}), &models));
    assert_eq!(resolved.spec.provider, "opencode-go");
    assert_eq!(resolved.spec.model_id, "kimi-k3");
    assert_eq!(resolved.spec.variant.as_deref(), Some("low"));
    assert!(resolved.selection.matched_fallback);
    assert_eq!(
        resolved.selection.fallback_entry,
        Some(entry(
            &[
                "kimi-coding",
                "kimi-for-coding",
                "moonshotai",
                "opencode-go"
            ],
            "kimi-k3",
            Some("low")
        ))
    );
}

#[test]
fn resolve_visual_engineering_glm_rung_keeps_max() {
    let models = registry(vec![model("zai-coding-plan", "glm-5.2")]);
    let resolved = expect_resolved(resolve("visual-engineering", json!({}), &models));
    assert_eq!(resolved.spec.provider, "zai-coding-plan");
    assert_eq!(resolved.spec.model_id, "glm-5.2");
    assert_eq!(resolved.spec.variant.as_deref(), Some("max"));
    assert!(resolved.selection.matched_fallback);
    assert_eq!(
        resolved.selection.fallback_entry,
        Some(entry(
            &["zai-coding-plan", "opencode-go", "vercel"],
            "glm-5.2",
            Some("max")
        ))
    );
}

#[test]
fn resolve_vercel_gpt56_keeps_native_top_rung() {
    for case in gpt56_category_cases() {
        let gateway_model_id = format!("openai/{}", case.model_id);
        let models = registry(vec![model("vercel", &gateway_model_id)]);
        let resolved = expect_resolved(resolve(case.category, json!({}), &models));
        assert_eq!(resolved.spec.provider, "vercel");
        assert_eq!(resolved.spec.model_id, gateway_model_id);
        assert_eq!(resolved.spec.variant.as_deref(), Some(case.native_variant));
        let fallback_entry = resolved.selection.fallback_entry.expect("fallback entry");
        assert_eq!(fallback_entry.model, case.model_id);
        assert_eq!(fallback_entry.variant.as_deref(), Some(case.native_variant));
    }
}

#[test]
fn resolve_vercel_and_copilot_gpt56_first_rung_provider_wins() {
    for case in gpt56_category_cases() {
        let gateway_model_id = format!("openai/{}", case.model_id);
        let models = registry(vec![
            model("github-copilot", case.model_id),
            model("vercel", &gateway_model_id),
        ]);
        let resolved = expect_resolved(resolve(case.category, json!({}), &models));
        let (provider, model_id, variant) = case.mixed_winner;
        assert_eq!(resolved.spec.provider, provider);
        assert_eq!(resolved.spec.model_id, model_id);
        assert_eq!(resolved.spec.variant.as_deref(), Some(variant));
        assert_eq!(
            resolved.selection.fallback_entry.expect("entry").model,
            case.model_id
        );
    }
}

#[test]
fn resolve_copilot_gpt56_uses_copilot_rung() {
    for case in gpt56_category_cases() {
        let models = registry(vec![model("github-copilot", case.model_id)]);
        let resolved = expect_resolved(resolve(case.category, json!({}), &models));
        assert_eq!(resolved.spec.provider, "github-copilot");
        assert_eq!(resolved.spec.model_id, case.model_id);
        assert_eq!(resolved.spec.variant.as_deref(), Some(case.copilot_variant));
        assert_eq!(
            resolved.selection.fallback_entry,
            Some(case.copilot_fallback_entry)
        );
    }
}

#[test]
fn resolve_deep_never_selects_retired_gpt55_rung() {
    let result = resolve(
        "deep",
        json!({}),
        &registry(vec![model("github-copilot", "gpt-5.5")]),
    );
    assert_eq!(result.kind(), "model_unavailable");
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
    let models = registry(vec![model("kimi-coding", "kimi-for-coding-highspeed")]);
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
            ("visual-engineering", "anthropic/claude-opus-5", Some("max")),
            ("artistry", "anthropic/claude-fable-5", Some("xhigh")),
            ("ultrabrain", "openai/gpt-5.6-sol", Some("max")),
            ("deep", "openai/gpt-5.6-sol", Some("medium")),
            ("quick", "kimi-coding/kimi-for-coding-highspeed", None),
            ("unspecified-low", "xai/grok-4.6", Some("xhigh")),
            ("architect", "anthropic/claude-fable-5", Some("xhigh")),
            ("unspecified-high", "kimi-coding/k3", Some("max")),
            ("writing", "kimi-coding/k3", Some("low")),
        ]
    );
    let mut requires = builtin_category_requires_model();
    requires.sort_unstable();
    assert_eq!(
        requires,
        vec![
            ("architect", "claude-fable-5"),
            ("deep", "gpt-5.6-sol"),
            ("ultrabrain", "gpt-5.6-sol")
        ]
    );
}

// ---- category/available-categories.test.ts ----

#[test]
fn available_exposes_architect_with_gate_model() {
    let names = resolve_available_category_names(
        &json!({}),
        &registry(vec![model("anthropic", "claude-fable-5")]),
    );
    assert!(names.contains(&"architect".to_string()));
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
    let config = json!({ "categories": { "architect": { "model": "anthropic/claude-fable-5" } } });
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
                model: "anthropic/claude-fable-5",
                variant: Some("xhigh")
            },
            Some("claude-fable-5")
        )
    );
}

// ---- category/category-routing-policy.test.ts ----

#[test]
fn routing_policy_pins_primary_models() {
    let config = |name: &str| builtin_category(name).expect("category").config.to_value();
    assert_eq!(
        [
            config("visual-engineering"),
            config("quick"),
            config("unspecified-high"),
            config("unspecified-low"),
        ],
        [
            json!({ "model": "anthropic/claude-opus-5", "variant": "max" }),
            json!({ "model": "kimi-coding/kimi-for-coding-highspeed" }),
            json!({ "model": "kimi-coding/k3", "variant": "max" }),
            json!({ "model": "xai/grok-4.6", "variant": "xhigh" }),
        ]
    );
}

#[test]
fn routing_policy_unspecified_low_chain_is_grok_first_without_luna() {
    let low = chain("unspecified-low");
    assert!(!low.iter().any(|rung| rung.model == "gpt-5.6-luna"));
    assert_eq!(
        low,
        vec![
            entry(
                &["xai", "github-copilot", "opencode", "vercel"],
                "grok-4.6",
                Some("xhigh")
            ),
            entry(OPENAI_RUNG, "gpt-5.6-terra", Some("high")),
            entry(
                &[
                    "anthropic",
                    "anthropic-api",
                    "github-copilot",
                    "opencode",
                    "vercel"
                ],
                "claude-sonnet-5",
                Some("low")
            ),
            entry(
                &[
                    "qwen-token-plan",
                    "alibaba-token-plan",
                    "qwen-token-plan-cn",
                    "alibaba-token-plan-cn"
                ],
                "qwen3.8-max-preview",
                Some("max")
            ),
            entry(
                &["deepseek", "opencode-go", "vercel"],
                "deepseek-v4-pro",
                Some("max")
            ),
            entry(
                &["xiaomi", "opencode-go", "vercel"],
                "mimo-v2.5-pro",
                Some("max")
            ),
        ]
    );
}

// ---- category/gpt-5.6-sol-routing.test.ts ----

fn assert_sol_rung(category: &str, variant: &str) {
    let resolved = expect_resolved(resolve(
        category,
        json!({}),
        &registry(vec![model("opencode", "gpt-5.6-sol")]),
    ));
    assert_eq!(
        (
            resolved.spec.provider.as_str(),
            resolved.spec.model_id.as_str(),
            resolved.spec.variant.as_deref()
        ),
        ("opencode", "gpt-5.6-sol", Some(variant))
    );
    assert_eq!(
        resolved.selection.fallback_entry.expect("entry").model,
        "gpt-5.6-sol"
    );
}

#[test]
fn sol_routing_ultrabrain_uses_migrated_rung() {
    assert_sol_rung("ultrabrain", "max");
}

#[test]
fn sol_routing_deep_uses_migrated_rung() {
    assert_sol_rung("deep", "medium");
}

#[test]
fn sol_routing_unspecified_low_is_unavailable_with_only_sol() {
    let result = resolve(
        "unspecified-low",
        json!({}),
        &registry(vec![model("opencode", "gpt-5.6-sol")]),
    );
    assert_eq!(result.kind(), "model_unavailable");
}

#[test]
fn sol_routing_unspecified_low_uses_grok_xhigh() {
    let resolved = expect_resolved(resolve(
        "unspecified-low",
        json!({}),
        &registry(vec![model("xai", "grok-4.6")]),
    ));
    assert_eq!(
        (
            resolved.spec.provider.as_str(),
            resolved.spec.model_id.as_str(),
            resolved.spec.variant.as_deref()
        ),
        ("xai", "grok-4.6", Some("xhigh"))
    );
}

#[test]
fn sol_routing_unspecified_low_uses_vercel_terra_high() {
    let models = registry(vec![model("vercel", "openai/gpt-5.6-terra")]);
    let resolved = expect_resolved(resolve("unspecified-low", json!({}), &models));
    assert_eq!(
        (
            resolved.spec.provider.as_str(),
            resolved.spec.model_id.as_str(),
            resolved.spec.variant.as_deref()
        ),
        ("vercel", "openai/gpt-5.6-terra", Some("high"))
    );
    assert_eq!(
        resolved.selection.fallback_entry.expect("entry").model,
        "gpt-5.6-terra"
    );
}

// ---- category/dead-chain.test.ts ----

fn opus_only() -> crate::host::fake::FakeRegistry {
    registry(vec![
        model("omo-mock", "mock-parent"),
        model("anthropic", "claude-opus-5"),
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
                "kimi-coding",
                "kimi-for-coding",
                "openai-codex",
                "deepseek",
                "qwen-token-plan",
                "alibaba-token-plan",
                "bailian-coding-plan",
                "vercel",
                "opencode-go",
                "xai",
                "anthropic-api",
                "github-copilot",
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
        &registry(vec![model("kimi-coding", "k3")]),
    );
    assert_eq!(result.kind(), "resolved");
    assert!(contains(result.available_categories(), "unspecified-high"));
    assert!(!contains(result.available_categories(), "quick"));
}

#[test]
fn dead_chain_gateway_unwrapped_id_keeps_category_available() {
    let result = resolve(
        "deep",
        json!({}),
        &registry(vec![model("vercel", "openai/gpt-5.6-sol")]),
    );
    assert_eq!(result.kind(), "resolved");
    assert!(contains(result.available_categories(), "deep"));
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
    assert!(!contains(result.available_categories(), "deep"));
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
    assert!(!contains(result.available_categories(), "deep"));
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
        Some("anthropic/claude-fable-5")
    );
    assert!(!contains(&unavailable.available_categories, "architect"));
}

#[test]
fn gating_architect_resolves_with_fable() {
    let mut models = pre_gating_models();
    models.push(model("anthropic", "claude-fable-5"));
    let resolved = expect_resolved(resolve("architect", json!({}), &registry(models)));
    assert_eq!(
        (
            resolved.spec.provider.as_str(),
            resolved.spec.model_id.as_str(),
            resolved.spec.variant.as_deref()
        ),
        ("anthropic", "claude-fable-5", Some("xhigh"))
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
        Some("openai/gpt-5.6-sol")
    );
    assert!(!contains(&unavailable.available_categories, "ultrabrain"));
}

#[test]
fn gating_ultrabrain_resolves_with_sol_at_max() {
    let resolved = expect_resolved(resolve(
        "ultrabrain",
        json!({}),
        &registry(vec![model("openai", "gpt-5.6-sol")]),
    ));
    assert_eq!(
        (
            resolved.spec.provider.as_str(),
            resolved.spec.model_id.as_str(),
            resolved.spec.variant.as_deref()
        ),
        ("openai", "gpt-5.6-sol", Some("max"))
    );
}

#[test]
fn gating_ultrabrain_copilot_sol_satisfies_gate() {
    let resolved = expect_resolved(resolve(
        "ultrabrain",
        json!({}),
        &registry(vec![model("github-copilot", "gpt-5.6-sol")]),
    ));
    assert_eq!(resolved.spec.provider, "github-copilot");
    assert_eq!(resolved.spec.variant.as_deref(), Some("max"));
}

#[test]
fn gating_deep_unavailable_on_cross_family_models() {
    let unavailable =
        expect_unavailable(resolve("deep", json!({}), &registry(pre_gating_models())));
    assert_eq!(
        unavailable.attempted_model.as_deref(),
        Some("openai/gpt-5.6-sol")
    );
    assert!(!contains(&unavailable.available_categories, "deep"));
}

#[test]
fn gating_deep_resolves_with_sol_at_medium() {
    let resolved = expect_resolved(resolve(
        "deep",
        json!({}),
        &registry(vec![model("openai", "gpt-5.6-sol")]),
    ));
    assert_eq!(
        (
            resolved.spec.provider.as_str(),
            resolved.spec.model_id.as_str(),
            resolved.spec.variant.as_deref()
        ),
        ("openai", "gpt-5.6-sol", Some("medium"))
    );
    assert!(contains(&resolved.available_categories, "deep"));
}

#[test]
fn gating_deep_explicit_entry_bypasses_gate() {
    let config = json!({ "categories": { "deep": { "model": "anthropic/claude-opus-5" } } });
    let resolved = expect_resolved(resolve(
        "deep",
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
        &registry(vec![model("kimi-coding", "k3")]),
    ));
    assert!(resolved.selection.matched_fallback);
    assert_eq!(resolved.spec.variant.as_deref(), Some("max"));
}

#[test]
fn gating_artistry_fable_is_primary_hit() {
    let resolved = expect_resolved(resolve(
        "artistry",
        json!({}),
        &registry(vec![model("anthropic", "claude-fable-5")]),
    ));
    assert!(!resolved.selection.matched_fallback);
}

#[test]
fn gating_unrelated_vendor_does_not_open_fable_gate() {
    let result = resolve(
        "architect",
        json!({}),
        &registry(vec![model("custom", "unrelated/claude-fable-5")]),
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
fn gating_real_vercel_gateway_id_opens_gate() {
    let result = resolve(
        "ultrabrain",
        json!({}),
        &registry(vec![model("vercel", "openai/gpt-5.6-sol")]),
    );
    assert_eq!(result.kind(), "resolved");
    assert!(contains(result.available_categories(), "ultrabrain"));
}

#[test]
fn gating_ungated_category_keeps_chain_fallback() {
    let resolved = expect_resolved(resolve(
        "quick",
        json!({}),
        &registry(vec![model("quotio-openai", "gpt-5.6-luna-fast")]),
    ));
    assert_eq!(resolved.spec.model_id, "gpt-5.6-luna-fast");
    assert!(contains(&resolved.available_categories, "quick"));
}

#[test]
fn gating_unmet_gates_leave_other_categories_listed() {
    let result = resolve(
        "quick",
        json!({}),
        &registry(vec![model("quotio-openai", "gpt-5.6-luna-fast")]),
    );
    let names = result.available_categories();
    assert!(!contains(names, "architect"));
    assert!(!contains(names, "ultrabrain"));
    assert!(!contains(names, "deep"));
    assert!(contains(names, "quick"));
}

// ---- category/resolve-category-boundary.test.ts ----

const QUICK_DEFAULT: &str = "kimi-coding/kimi-for-coding-highspeed";

fn quick_model() -> Value {
    model("kimi-coding", "kimi-for-coding-highspeed")
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
        "provider": "kimi-coding", "id": "kimi-for-coding-highspeed", "headers": { "User-Agent": "test" }
    });
    let resolved = expect_resolved(resolve(
        "quick",
        json!({}),
        &registry(vec![header_model.clone()]),
    ));
    assert_eq!(resolved.spec.provider, "kimi-coding");
    assert_eq!(resolved.spec.model_id, "kimi-for-coding-highspeed");
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
    let entry = json!({ "provider": { "marker": "hidden available accessor marker" }, "id": "kimi-for-coding-highspeed" });
    let result = resolve("quick", json!({}), &fixed_registry(json!([entry]), None));
    assert_sanitized(result, &[], &["hidden available accessor marker"]);
}

#[test]
fn boundary_malformed_find_results_are_sanitized() {
    let malformed = [
        json!({}),
        json!({ "provider": { "secret": "hidden" }, "id": ["kimi-for-coding-highspeed"] }),
        json!({ "provider": "kimi-coding", "id": "kimi-for-coding-highspeed", "password": "hidden" }),
        json!({ "provider": "kimi-coding", "id": "kimi-for-coding-highspeed", "accessToken": "hidden" }),
        json!({ "provider": "kimi-coding", "id": "kimi-for-coding-highspeed", "privateToken": "hidden" }),
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
        json!({ "provider": ["hidden find accessor marker"], "id": "kimi-for-coding-highspeed" });
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
        json!({ "provider": "kimi-coding", "id": "" }),
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
        "provider": "kimi-coding", "id": "kimi-for-coding-highspeed", "privateToken": "hidden"
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
        json!("kimi-coding/kimi-for-coding-highspeed"),
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
