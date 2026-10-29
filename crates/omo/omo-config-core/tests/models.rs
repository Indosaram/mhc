use omo_config_core::internal::validate::safe_parse;
use omo_config_core::{
    excluded_model_pattern, is_excluded_model, omo_config_schema, resolve_model_references,
};
use serde_json::{Value, json};

fn view(value: Value) -> Value {
    safe_parse(&omo_config_schema(), &value).expect("config")
}

#[test]
fn resolve_model_references_pulls_ids_and_unset_tuning_from_the_catalog_without_mutating_the_view()
{
    let original = view(json!({
        "models": { "sol": { "model": "openai/gpt-5.6-sol", "variant": "high", "reasoningEffort": "xhigh" } },
        "agents": { "oracle": { "model": "sol", "models": ["sol"] } },
        "categories": { "deep": { "model": "sol", "fallback_models": ["sol"] } },
    }));
    let snapshot = original.clone();
    let result = resolve_model_references(&original);
    assert_eq!(original, snapshot);
    assert_eq!(result.diagnostics, vec![]);
    assert_eq!(
        result.view["agents"]["oracle"],
        json!({
            "model": "openai/gpt-5.6-sol",
            "reasoning": "xhigh",
            "models": [{ "model": "openai/gpt-5.6-sol", "reasoning": "xhigh" }],
        })
    );
    assert_eq!(
        result.view["categories"]["deep"],
        json!({
            "model": "openai/gpt-5.6-sol",
            "reasoning": "xhigh",
            "fallback_models": [{ "model": "openai/gpt-5.6-sol", "reasoning": "xhigh" }],
        })
    );
}

#[test]
fn resolve_model_references_lets_site_local_tuning_win() {
    let result = resolve_model_references(&view(json!({
        "models": { "sol": { "model": "openai/gpt-5.6-sol", "variant": "high", "reasoningEffort": "xhigh" } },
        "agents": {
            "oracle": {
                "model": "sol",
                "variant": "low",
                "reasoningEffort": "minimal",
                "models": [{ "model": "sol", "variant": "medium", "reasoningEffort": "high" }],
            }
        },
        "categories": {
            "deep": { "fallback_models": [{ "model": "sol", "variant": "medium", "reasoningEffort": "high" }] }
        },
    })));
    assert_eq!(
        result.view["agents"]["oracle"]["model"],
        json!("openai/gpt-5.6-sol")
    );
    assert_eq!(
        result.view["agents"]["oracle"]["reasoning"],
        json!("minimal")
    );
    assert_eq!(
        result.view["agents"]["oracle"]["models"],
        json!([{ "model": "openai/gpt-5.6-sol", "reasoning": "high" }])
    );
    assert_eq!(
        result.view["categories"]["deep"]["fallback_models"],
        json!([{ "model": "openai/gpt-5.6-sol", "reasoning": "high" }])
    );
}

#[test]
fn resolve_model_references_passes_names_outside_the_catalog_through_verbatim() {
    let result = resolve_model_references(&view(json!({
        "models": { "sol": { "model": "openai/gpt-5.6-sol" } },
        "agents": { "oracle": { "model": "anthropic/claude", "models": ["openai/gpt-5"] } },
        "categories": { "deep": { "model": "anthropic/claude", "fallback_models": ["openai/gpt-5"] } },
    })));
    assert_eq!(result.diagnostics, vec![]);
    assert_eq!(
        result.view["agents"]["oracle"]["model"],
        json!("anthropic/claude")
    );
    assert_eq!(
        result.view["agents"]["oracle"]["models"],
        json!(["openai/gpt-5"])
    );
    assert_eq!(
        result.view["categories"]["deep"]["model"],
        json!("anthropic/claude")
    );
    assert_eq!(
        result.view["categories"]["deep"]["fallback_models"],
        json!(["openai/gpt-5"])
    );
}

#[test]
fn resolve_model_references_reports_a_self_referential_catalog_entry_without_hanging() {
    let result = resolve_model_references(&view(json!({
        "models": { "a": { "model": "a" } },
        "agents": { "oracle": { "model": "a" } },
    })));
    assert_eq!(result.diagnostics.len(), 1);
    let diagnostic = &result.diagnostics[0];
    assert_eq!(diagnostic.kind, "model_catalog_cycle");
    assert_eq!(
        diagnostic.message,
        "Model catalog entry \"a\" references itself"
    );
    assert_eq!(diagnostic.path, "models.a.model");
    assert_eq!(result.view["agents"]["oracle"]["model"], json!("a"));
}

#[test]
fn find_model_catalog_cycles_reports_every_member_of_a_cycle_sorted() {
    let catalog = json!({
        "b": { "model": "c" },
        "c": { "model": "b" },
        "a": { "model": "openai/gpt-5" },
    })
    .as_object()
    .cloned()
    .expect("catalog");
    assert_eq!(
        omo_config_core::find_model_catalog_cycles(&catalog),
        vec!["b".to_string(), "c".to_string()]
    );
}

#[test]
fn resolve_model_references_expands_profiles_and_reports_shadowing_and_splicing() {
    let result = resolve_model_references(&view(json!({
        "models": {
            "shared": { "model": "openai/gpt-5", "reasoning": "high" },
            "clash": { "model": "anthropic/claude" },
        },
        "model_profiles": {
            "daily": { "models": ["shared"] },
            "clash": { "models": ["openai/gpt-4"] },
        },
        "agents": { "worker": { "models": ["daily"] } },
        "categories": { "quick": { "models": ["daily"] } },
    })));
    assert_eq!(result.view["model_profiles"]["daily"]["models"], json!([{ "model": "openai/gpt-5", "reasoning": "high" }]));
    assert_eq!(result.diagnostics.len(), 3);
    assert!(result.diagnostics.iter().any(|item| item.path == "model_profiles.clash"));
    assert!(result.diagnostics.iter().any(|item| item.path == "agents.worker.models.0"));
    assert!(result.diagnostics.iter().any(|item| item.path == "categories.quick.models.0"));
}

fn pattern_list(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}

#[test]
fn excluded_model_pattern_matches_a_bare_model_id_on_every_provider() {
    let patterns = pattern_list(&["gpt-6.1-sol"]);

    assert_eq!(
        excluded_model_pattern("anthropic/gpt-6.1-sol", Some(&patterns)),
        Some("gpt-6.1-sol".to_string())
    );
    assert_eq!(
        excluded_model_pattern("github-copilot/gpt-6.1-sol", Some(&patterns)),
        Some("gpt-6.1-sol".to_string())
    );
    assert!(!is_excluded_model("anthropic/claude", Some(&patterns)));
}

#[test]
fn excluded_model_pattern_matches_a_provider_scoped_entry_on_that_provider_only() {
    let patterns = pattern_list(&["opencode/"]);

    assert!(is_excluded_model("opencode/", Some(&patterns)));
    assert!(!is_excluded_model("anthropic/", Some(&patterns)));
}

#[test]
fn excluded_model_pattern_matches_a_wildcard_case_insensitively() {
    let patterns = pattern_list(&["CLAUDE*"]);

    assert!(is_excluded_model("anthropic/claude", Some(&patterns)));
    assert!(is_excluded_model("anthropic-subscription/claude", Some(&patterns)));
    assert!(!is_excluded_model("anthropic/gpt-6.1-sol", Some(&patterns)));
}

#[test]
fn excluded_model_pattern_matches_every_tail_of_a_nested_gateway_selector() {
    let nested = "openrouter/anthropic/";

    assert!(is_excluded_model(nested, Some(&pattern_list(&["*"]))));
    assert!(is_excluded_model(nested, Some(&pattern_list(&["anthropic/"]))));
    assert!(is_excluded_model(
        nested,
        Some(&pattern_list(&["openrouter/anthropic/"]))
    ));
    assert!(!is_excluded_model(nested, Some(&pattern_list(&["openrouter/"]))));
    assert!(!is_excluded_model(nested, Some(&pattern_list(&["anthropic"]))));
}

#[test]
fn excluded_model_pattern_treats_regex_syntax_as_literal_and_skips_blank_entries() {
    let patterns = pattern_list(&["  ", "gpt-6.1-sol"]);

    assert!(is_excluded_model("openai/gpt-6.1-sol", Some(&patterns)));
    assert!(!is_excluded_model("openai/gpt-6x1-sol", Some(&patterns)));
    assert!(!is_excluded_model("openai/gpt-6.1-sol", None));
}

#[test]
fn excluded_model_pattern_returns_the_first_matching_entry_verbatim() {
    let patterns = pattern_list(&["  ", " gpt-6.1-sol ", "gpt-*"]);

    assert_eq!(
        excluded_model_pattern("openai/gpt-6.1-sol", Some(&patterns)),
        Some(" gpt-6.1-sol ".to_string())
    );
    assert_eq!(
        excluded_model_pattern("openai/gpt-5", Some(&patterns)),
        Some("gpt-*".to_string())
    );
}
