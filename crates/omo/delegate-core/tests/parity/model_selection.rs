use delegate_core::DelegateFallbackEntry;
use delegate_core::DelegateModelResolutionDeps;
use delegate_core::DelegateModelResolutionInput;
use delegate_core::DelegateModelResolutionResult;
use delegate_core::ResolvedDelegateModel;
use delegate_core::resolve_model_for_delegate_task;
use indexmap::IndexSet;
use pretty_assertions::assert_eq;

fn no_cache_deps() -> DelegateModelResolutionDeps<'static> {
    DelegateModelResolutionDeps::default()
}

fn entry(providers: &[&str], model: &str, variant: Option<&str>) -> DelegateFallbackEntry {
    DelegateFallbackEntry {
        providers: providers.iter().map(ToString::to_string).collect(),
        model: model.to_string(),
        variant: variant.map(ToString::to_string),
    }
}

fn native_sol_entry() -> DelegateFallbackEntry {
    entry(&["openai", "vercel"], "gpt-5.6-sol", Some("xhigh"))
}

fn copilot_sol_entry() -> DelegateFallbackEntry {
    entry(&["github-copilot"], "gpt-5.6-sol", Some("high"))
}

fn legacy_gpt_entry() -> DelegateFallbackEntry {
    entry(
        &["openai", "github-copilot", "opencode", "vercel"],
        "gpt-5.5",
        Some("xhigh"),
    )
}

fn gpt56_sol_fallback_chain() -> Vec<DelegateFallbackEntry> {
    vec![native_sol_entry(), copilot_sol_entry(), legacy_gpt_entry()]
}

fn models(ids: &[&str]) -> IndexSet<String> {
    ids.iter().map(ToString::to_string).collect()
}

fn chain_match(
    model: &str,
    variant: &str,
    fallback_entry: DelegateFallbackEntry,
) -> Option<DelegateModelResolutionResult> {
    Some(DelegateModelResolutionResult::Resolved(
        ResolvedDelegateModel {
            model: model.to_string(),
            variant: Some(variant.to_string()),
            fallback_entry: Some(fallback_entry),
            matched_fallback: true,
        },
    ))
}

fn resolve_chain(available: &[&str]) -> Option<DelegateModelResolutionResult> {
    resolve_model_for_delegate_task(
        &DelegateModelResolutionInput {
            available_models: models(available),
            fallback_chain: Some(gpt56_sol_fallback_chain()),
            ..Default::default()
        },
        &no_cache_deps(),
    )
}

#[test]
fn no_provider_cache_without_user_override_returns_skipped_sentinel() {
    let result = resolve_model_for_delegate_task(
        &DelegateModelResolutionInput {
            category_default_model: Some("openai/gpt-5.4".to_string()),
            ..Default::default()
        },
        &no_cache_deps(),
    );

    assert_eq!(result, Some(DelegateModelResolutionResult::Skipped));
}

#[test]
fn unreachable_user_primary_promotes_reachable_fallback_with_variant() {
    let result = resolve_model_for_delegate_task(
        &DelegateModelResolutionInput {
            user_model: Some("quotio/claude-haiku-4-5-unavailable".to_string()),
            user_fallback_models: Some(vec!["openai/gpt-5.4 high".to_string()]),
            available_models: models(&["openai/gpt-5.4-preview"]),
            ..Default::default()
        },
        &no_cache_deps(),
    );

    assert_eq!(
        result,
        Some(DelegateModelResolutionResult::Resolved(
            ResolvedDelegateModel {
                model: "openai/gpt-5.4-preview".to_string(),
                variant: Some("high".to_string()),
                fallback_entry: None,
                matched_fallback: true,
            }
        ))
    );
}

#[test]
fn connected_providers_cache_selects_first_connected_fallback_provider() {
    let connected = vec!["openai".to_string()];
    let result = resolve_model_for_delegate_task(
        &DelegateModelResolutionInput {
            fallback_chain: Some(vec![
                entry(&["anthropic"], "claude-sonnet-4-6", None),
                entry(&["openai"], "gpt-5.4", Some("medium")),
            ]),
            ..Default::default()
        },
        &DelegateModelResolutionDeps {
            connected_providers: Some(&connected),
            has_provider_models_cache: true,
            has_connected_providers_cache: true,
            log: None,
        },
    );

    assert_eq!(
        result,
        chain_match(
            "openai/gpt-5.4",
            "medium",
            entry(&["openai"], "gpt-5.4", Some("medium")),
        )
    );
}

#[test]
fn only_transformed_vercel_gpt56_sol_keeps_native_xhigh_rung() {
    let result = resolve_model_for_delegate_task(
        &DelegateModelResolutionInput {
            available_models: models(&["vercel/openai/gpt-5.6-sol"]),
            fallback_chain: Some(gpt56_sol_fallback_chain()),
            system_default_model: Some("system/default".to_string()),
            ..Default::default()
        },
        &no_cache_deps(),
    );

    assert_eq!(
        result,
        chain_match("vercel/openai/gpt-5.6-sol", "xhigh", native_sol_entry())
    );
}

#[test]
fn transformed_vercel_and_copilot_gpt56_sol_vercel_xhigh_wins() {
    assert_eq!(
        resolve_chain(&["github-copilot/gpt-5.6-sol", "vercel/openai/gpt-5.6-sol"]),
        chain_match("vercel/openai/gpt-5.6-sol", "xhigh", native_sol_entry())
    );
}

#[test]
fn only_copilot_gpt56_sol_dedicated_high_rung_wins() {
    assert_eq!(
        resolve_chain(&["github-copilot/gpt-5.6-sol"]),
        chain_match("github-copilot/gpt-5.6-sol", "high", copilot_sol_entry())
    );
}

#[test]
fn gpt56_unavailable_with_copilot_gpt55_keeps_legacy_fallback() {
    assert_eq!(
        resolve_chain(&["github-copilot/gpt-5.5"]),
        chain_match("github-copilot/gpt-5.5", "xhigh", legacy_gpt_entry())
    );
}

#[test]
fn fallback_model_through_unlisted_provider_keeps_cross_provider_matching() {
    let fallback_entry = entry(&["openai"], "gpt-5.5", Some("high"));
    let result = resolve_model_for_delegate_task(
        &DelegateModelResolutionInput {
            available_models: models(&["custom-provider/gpt-5.5"]),
            fallback_chain: Some(vec![fallback_entry.clone()]),
            ..Default::default()
        },
        &no_cache_deps(),
    );

    assert_eq!(
        result,
        chain_match("custom-provider/gpt-5.5", "high", fallback_entry)
    );
}

#[test]
fn custom_and_later_rung_providers_same_model_custom_keeps_earlier_variant() {
    assert_eq!(
        resolve_chain(&[
            "long-custom-provider/gpt-5.6-sol",
            "github-copilot/gpt-5.6-sol",
        ]),
        chain_match(
            "long-custom-provider/gpt-5.6-sol",
            "xhigh",
            native_sol_entry()
        )
    );
}
