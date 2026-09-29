use omo_config_core::internal::validate::safe_parse;
use omo_config_core::{omo_config_schema, resolve_model_references};
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
