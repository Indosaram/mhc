use model_core::ExtendedModelResolutionInput;
use model_core::FallbackEntry;
use model_core::ModelResolutionInput;
use model_core::ModelResolutionProvenance;
use model_core::ModelResolutionResult;
use model_core::resolve_model;
use model_core::resolve_model_with_fallback;
use model_core::resolve_model_with_fallback_using;
use pretty_assertions::assert_eq;
use serde_json::json;

use crate::support::ConnectedCache;
use crate::support::LogCapture;
use crate::support::entry;
use crate::support::set;

fn some(value: &str) -> Option<String> {
    Some(value.to_string())
}

fn resolve_input(
    user_model: Option<&str>,
    inherited_model: Option<&str>,
    system_default: Option<&str>,
) -> Option<String> {
    resolve_model(&ModelResolutionInput {
        user_model: user_model.map(str::to_string),
        inherited_model: inherited_model.map(str::to_string),
        system_default: system_default.map(str::to_string),
    })
}

fn resolved(input: &ExtendedModelResolutionInput) -> ModelResolutionResult {
    resolve_model_with_fallback(input).expect("expected model resolution result")
}

fn resolved_with(
    input: &ExtendedModelResolutionInput,
    cache: &ConnectedCache,
) -> ModelResolutionResult {
    resolve_model_with_fallback_using(input, cache).expect("expected model resolution result")
}

fn chain(entries: &[(&[&str], &str)]) -> Option<Vec<FallbackEntry>> {
    Some(
        entries
            .iter()
            .map(|(providers, model)| entry(providers, model, None))
            .collect(),
    )
}

// ---- resolveModel ----

#[test]
fn returns_user_model_when_all_three_are_set() {
    assert_eq!(
        resolve_input(
            Some("anthropic/claude-opus-4-7"),
            Some("openai/gpt-5.4"),
            Some("google/gemini-3.1-pro")
        ),
        some("anthropic/claude-opus-4-7")
    );
}

#[test]
fn returns_inherited_model_when_user_model_is_undefined() {
    assert_eq!(
        resolve_input(None, Some("openai/gpt-5.4"), Some("google/gemini-3.1-pro")),
        some("openai/gpt-5.4")
    );
}

#[test]
fn returns_system_default_when_both_user_model_and_inherited_model_are_undefined() {
    assert_eq!(
        resolve_input(None, None, Some("google/gemini-3.1-pro")),
        some("google/gemini-3.1-pro")
    );
}

#[test]
fn treats_empty_string_as_unset_and_uses_fallback() {
    assert_eq!(
        resolve_input(
            Some(""),
            Some("openai/gpt-5.4"),
            Some("google/gemini-3.1-pro")
        ),
        some("openai/gpt-5.4")
    );
}

#[test]
fn treats_whitespace_only_string_as_unset_and_uses_fallback() {
    assert_eq!(
        resolve_input(Some("   "), Some(""), Some("google/gemini-3.1-pro")),
        some("google/gemini-3.1-pro")
    );
}

#[test]
fn same_input_returns_same_output() {
    let first = resolve_input(
        Some("anthropic/claude-opus-4-7"),
        Some("openai/gpt-5.4"),
        Some("google/gemini-3.1-pro"),
    );
    let second = resolve_input(
        Some("anthropic/claude-opus-4-7"),
        Some("openai/gpt-5.4"),
        Some("google/gemini-3.1-pro"),
    );
    assert_eq!(first, second);
}

// ---- Step 1: UI selection ----

#[test]
fn returns_ui_selected_model_with_override_source_when_provided() {
    let log = LogCapture::install();
    let input = ExtendedModelResolutionInput {
        ui_selected_model: some("opencode/big-pickle"),
        user_model: some("anthropic/claude-opus-4-7"),
        fallback_chain: chain(&[(&["anthropic", "github-copilot"], "claude-opus-4-7")]),
        available_models: set(&[
            "anthropic/claude-opus-4-7",
            "github-copilot/claude-opus-4-7-preview",
        ]),
        system_default_model: some("google/gemini-3.1-pro"),
        ..Default::default()
    };

    let result = resolved(&input);

    assert_eq!(result.model, "opencode/big-pickle");
    assert_eq!(result.source, ModelResolutionProvenance::Override);
    assert!(log.was_called_with(
        "Model resolved via UI selection",
        Some(&json!({ "model": "opencode/big-pickle" }))
    ));
}

#[test]
fn ui_selection_takes_priority_over_config_override() {
    let _log = LogCapture::install();
    let input = ExtendedModelResolutionInput {
        ui_selected_model: some("opencode/big-pickle"),
        user_model: some("anthropic/claude-opus-4-7"),
        available_models: set(&["anthropic/claude-opus-4-7"]),
        system_default_model: some("google/gemini-3.1-pro"),
        ..Default::default()
    };

    let result = resolved(&input);

    assert_eq!(result.model, "opencode/big-pickle");
    assert_eq!(result.source, ModelResolutionProvenance::Override);
}

#[test]
fn whitespace_only_ui_selected_model_is_treated_as_not_provided() {
    let log = LogCapture::install();
    let input = ExtendedModelResolutionInput {
        ui_selected_model: some("   "),
        user_model: some("anthropic/claude-opus-4-7"),
        available_models: set(&["anthropic/claude-opus-4-7"]),
        system_default_model: some("google/gemini-3.1-pro"),
        ..Default::default()
    };

    let result = resolved(&input);

    assert_eq!(result.model, "anthropic/claude-opus-4-7");
    assert!(log.was_called_with(
        "Model resolved via config override",
        Some(&json!({ "model": "anthropic/claude-opus-4-7" }))
    ));
}

#[test]
fn empty_string_ui_selected_model_falls_through_to_config_override() {
    let _log = LogCapture::install();
    let input = ExtendedModelResolutionInput {
        ui_selected_model: some(""),
        user_model: some("anthropic/claude-opus-4-7"),
        available_models: set(&["anthropic/claude-opus-4-7"]),
        system_default_model: some("google/gemini-3.1-pro"),
        ..Default::default()
    };

    assert_eq!(resolved(&input).model, "anthropic/claude-opus-4-7");
}

// ---- Step 2: config override ----

#[test]
fn returns_user_model_with_override_source_when_user_model_is_provided() {
    let log = LogCapture::install();
    let input = ExtendedModelResolutionInput {
        user_model: some("anthropic/claude-opus-4-7"),
        fallback_chain: chain(&[(&["anthropic", "github-copilot"], "claude-opus-4-7")]),
        available_models: set(&[
            "anthropic/claude-opus-4-7",
            "github-copilot/claude-opus-4-7-preview",
        ]),
        system_default_model: some("google/gemini-3.1-pro"),
        ..Default::default()
    };

    let result = resolved(&input);

    assert_eq!(result.model, "anthropic/claude-opus-4-7");
    assert_eq!(result.source, ModelResolutionProvenance::Override);
    assert!(log.was_called_with(
        "Model resolved via config override",
        Some(&json!({ "model": "anthropic/claude-opus-4-7" }))
    ));
}

#[test]
fn override_takes_priority_even_if_model_not_in_available_models() {
    let _log = LogCapture::install();
    let input = ExtendedModelResolutionInput {
        user_model: some("custom/my-model"),
        fallback_chain: chain(&[(&["anthropic"], "claude-opus-4-7")]),
        available_models: set(&["anthropic/claude-opus-4-7"]),
        system_default_model: some("google/gemini-3.1-pro"),
        ..Default::default()
    };

    let result = resolved(&input);

    assert_eq!(result.model, "custom/my-model");
    assert_eq!(result.source, ModelResolutionProvenance::Override);
}

fn blank_user_model_input(user_model: &str) -> ExtendedModelResolutionInput {
    ExtendedModelResolutionInput {
        user_model: some(user_model),
        fallback_chain: chain(&[(&["anthropic"], "claude-opus-4-7")]),
        available_models: set(&["anthropic/claude-opus-4-7"]),
        system_default_model: some("google/gemini-3.1-pro"),
        ..Default::default()
    }
}

#[test]
fn whitespace_only_user_model_is_treated_as_not_provided() {
    let _log = LogCapture::install();
    assert_ne!(
        resolved(&blank_user_model_input("   ")).source,
        ModelResolutionProvenance::Override
    );
}

#[test]
fn empty_string_user_model_is_treated_as_not_provided() {
    let _log = LogCapture::install();
    assert_ne!(
        resolved(&blank_user_model_input("")).source,
        ModelResolutionProvenance::Override
    );
}

// ---- Step 3: provider fallback chain ----

fn chain_input(
    entries: &[(&[&str], &str)],
    available: &[&str],
    system_default: Option<&str>,
) -> ExtendedModelResolutionInput {
    ExtendedModelResolutionInput {
        fallback_chain: chain(entries),
        available_models: set(available),
        system_default_model: system_default.map(str::to_string),
        ..Default::default()
    }
}

fn assert_provider_fallback(result: &ModelResolutionResult, model: &str) {
    assert_eq!(result.model, model);
    assert_eq!(result.source, ModelResolutionProvenance::ProviderFallback);
}

fn assert_system_default(result: &ModelResolutionResult, model: &str) {
    assert_eq!(result.model, model);
    assert_eq!(result.source, ModelResolutionProvenance::SystemDefault);
}

#[test]
fn tries_providers_in_order_within_entry_and_returns_first_match() {
    let log = LogCapture::install();
    let input = chain_input(
        &[(
            &["anthropic", "github-copilot", "opencode"],
            "claude-opus-4-7",
        )],
        &[
            "github-copilot/claude-opus-4-7-preview",
            "opencode/claude-opus-4-7",
        ],
        Some("google/gemini-3.1-pro"),
    );

    let result = resolved(&input);

    assert_provider_fallback(&result, "github-copilot/claude-opus-4-7-preview");
    assert!(log.was_called_with(
        "Model resolved via fallback chain (availability confirmed)",
        Some(&json!({
            "provider": "github-copilot",
            "model": "claude-opus-4-7",
            "match": "github-copilot/claude-opus-4-7-preview",
            "variant": null,
        }))
    ));
}

#[test]
fn respects_provider_priority_order_within_entry() {
    let _log = LogCapture::install();
    let input = chain_input(
        &[(&["openai", "anthropic", "google"], "gpt-5.4")],
        &[
            "openai/gpt-5.4",
            "anthropic/claude-opus-4-7",
            "google/gemini-3.1-pro",
        ],
        Some("google/gemini-3.1-pro"),
    );

    assert_provider_fallback(&resolved(&input), "openai/gpt-5.4");
}

#[test]
fn tries_next_provider_when_first_provider_has_no_match() {
    let _log = LogCapture::install();
    let input = chain_input(
        &[(&["anthropic", "opencode"], "gpt-5-nano")],
        &["opencode/gpt-5-nano"],
        Some("google/gemini-3.1-pro"),
    );

    assert_provider_fallback(&resolved(&input), "opencode/gpt-5-nano");
}

#[test]
fn uses_fuzzy_matching_within_provider() {
    let _log = LogCapture::install();
    let input = chain_input(
        &[(&["anthropic", "github-copilot"], "claude-opus")],
        &[
            "anthropic/claude-opus-4-7",
            "github-copilot/claude-opus-4-7-preview",
        ],
        Some("google/gemini-3.1-pro"),
    );

    assert_provider_fallback(&resolved(&input), "anthropic/claude-opus-4-7");
}

#[test]
fn skips_fallback_chain_when_not_provided() {
    let _log = LogCapture::install();
    let input = ExtendedModelResolutionInput {
        available_models: set(&["anthropic/claude-opus-4-7"]),
        system_default_model: some("google/gemini-3.1-pro"),
        ..Default::default()
    };

    assert_eq!(
        resolved(&input).source,
        ModelResolutionProvenance::SystemDefault
    );
}

#[test]
fn skips_fallback_chain_when_empty() {
    let _log = LogCapture::install();
    let input = chain_input(
        &[],
        &["anthropic/claude-opus-4-7"],
        Some("google/gemini-3.1-pro"),
    );

    assert_eq!(
        resolved(&input).source,
        ModelResolutionProvenance::SystemDefault
    );
}

#[test]
fn case_insensitive_fuzzy_matching() {
    let _log = LogCapture::install();
    let input = chain_input(
        &[(&["anthropic"], "CLAUDE-OPUS")],
        &["anthropic/claude-opus-4-7"],
        Some("google/gemini-3.1-pro"),
    );

    assert_provider_fallback(&resolved(&input), "anthropic/claude-opus-4-7");
}

#[test]
fn cross_provider_match_tries_next_entry_if_no_match_found_anywhere() {
    let _log = LogCapture::install();
    let input = chain_input(
        &[
            (&["zai-coding-plan"], "nonexistent-model"),
            (&["anthropic"], "claude-sonnet-4-6"),
        ],
        &["anthropic/claude-sonnet-4-6"],
        Some("google/gemini-3.1-pro"),
    );

    assert_provider_fallback(&resolved(&input), "anthropic/claude-sonnet-4-6");
}

// ---- Step 4: system default ----

#[test]
fn returns_system_default_when_no_availability_match_found_in_fallback_chain() {
    let log = LogCapture::install();
    let input = chain_input(
        &[(&["anthropic"], "nonexistent-model")],
        &["openai/gpt-5.4", "anthropic/claude-opus-4-7"],
        Some("google/gemini-3.1-pro"),
    );

    assert_system_default(&resolved(&input), "google/gemini-3.1-pro");
    assert!(log.was_called_with(
        "No available model found in fallback chain, falling through to system default",
        None
    ));
}

#[test]
fn returns_none_when_available_models_empty_and_no_connected_providers_cache_exists() {
    let _log = LogCapture::install();
    let input = chain_input(&[(&["anthropic"], "claude-opus-4-7")], &[], None);

    assert_eq!(
        resolve_model_with_fallback_using(&input, &ConnectedCache(None)),
        None
    );
}

#[test]
fn uses_connected_provider_from_fallback_when_available_models_empty_but_cache_exists() {
    let _log = LogCapture::install();
    let input = chain_input(
        &[(&["anthropic", "openai"], "claude-opus-4-7")],
        &[],
        Some("google/gemini-3.1-pro"),
    );

    assert_provider_fallback(
        &resolved_with(&input, &ConnectedCache::with(&["openai", "google"])),
        "openai/claude-opus-4-7",
    );
}

#[test]
fn uses_github_copilot_when_google_not_connected() {
    let _log = LogCapture::install();
    let input = chain_input(
        &[(&["google", "github-copilot", "opencode"], "gemini-3.1-pro")],
        &[],
        Some("anthropic/claude-sonnet-4-6"),
    );

    assert_provider_fallback(
        &resolved_with(&input, &ConnectedCache::with(&["github-copilot"])),
        "github-copilot/gemini-3.1-pro-preview",
    );
}

#[test]
fn falls_through_to_system_default_when_no_provider_in_fallback_is_connected() {
    let _log = LogCapture::install();
    let input = chain_input(
        &[(&["openai", "opencode"], "claude-haiku-4-5")],
        &[],
        Some("anthropic/claude-opus-4-7-20251101"),
    );

    assert_system_default(
        &resolved_with(&input, &ConnectedCache::with(&["anthropic"])),
        "anthropic/claude-opus-4-7-20251101",
    );
}

#[test]
fn falls_through_to_system_default_when_no_cache_and_system_default_model_is_provided() {
    let _log = LogCapture::install();
    let input = chain_input(
        &[(&["anthropic"], "claude-opus-4-7")],
        &[],
        Some("google/gemini-3.1-pro"),
    );

    assert_system_default(
        &resolved_with(&input, &ConnectedCache(None)),
        "google/gemini-3.1-pro",
    );
}

#[test]
fn returns_system_default_when_fallback_chain_is_not_provided() {
    let _log = LogCapture::install();
    let input = ExtendedModelResolutionInput {
        available_models: set(&["openai/gpt-5.4"]),
        system_default_model: some("google/gemini-3.1-pro"),
        ..Default::default()
    };

    assert_system_default(&resolved(&input), "google/gemini-3.1-pro");
}

// ---- Multi-entry fallback chain ----

#[test]
fn resolves_to_claude_opus_when_openai_unavailable_but_anthropic_available() {
    let _log = LogCapture::install();
    let input = ExtendedModelResolutionInput {
        fallback_chain: Some(vec![
            entry(
                &["openai", "github-copilot", "opencode"],
                "gpt-5.4",
                Some("high"),
            ),
            entry(
                &["anthropic", "github-copilot", "opencode"],
                "claude-opus-4-7",
                Some("max"),
            ),
        ]),
        available_models: set(&["anthropic/claude-opus-4-7"]),
        system_default_model: some("system/default"),
        ..Default::default()
    };

    assert_provider_fallback(&resolved(&input), "anthropic/claude-opus-4-7");
}

#[test]
fn tries_all_providers_in_first_entry_before_moving_to_second_entry() {
    let _log = LogCapture::install();
    let input = chain_input(
        &[
            (&["openai", "anthropic"], "gpt-5.4"),
            (&["google"], "gemini-3.1-pro"),
        ],
        &["google/gemini-3.1-pro"],
        Some("system/default"),
    );

    assert_provider_fallback(&resolved(&input), "google/gemini-3.1-pro");
}

#[test]
fn returns_first_matching_entry_even_if_later_entries_have_better_matches() {
    let _log = LogCapture::install();
    let input = chain_input(
        &[
            (&["openai"], "gpt-5.4"),
            (&["anthropic"], "claude-opus-4-7"),
        ],
        &["openai/gpt-5.4", "anthropic/claude-opus-4-7"],
        Some("system/default"),
    );

    assert_provider_fallback(&resolved(&input), "openai/gpt-5.4");
}

#[test]
fn falls_through_to_system_default_when_none_match_availability() {
    let _log = LogCapture::install();
    let input = chain_input(
        &[
            (&["openai"], "gpt-5.4"),
            (&["anthropic"], "claude-opus-4-7"),
            (&["google"], "gemini-3.1-pro"),
        ],
        &["other/model"],
        Some("system/default"),
    );

    assert_system_default(&resolved(&input), "system/default");
}

// ---- Type safety ----

#[test]
fn result_has_correct_model_resolution_result_shape() {
    let _log = LogCapture::install();
    let input = ExtendedModelResolutionInput {
        user_model: some("anthropic/claude-opus-4-7"),
        system_default_model: some("google/gemini-3.1-pro"),
        ..Default::default()
    };

    let result = resolved(&input);

    assert!(!result.model.is_empty());
    assert!(matches!(
        result.source,
        ModelResolutionProvenance::Override
            | ModelResolutionProvenance::ProviderFallback
            | ModelResolutionProvenance::SystemDefault
    ));
}

// ---- categoryDefaultModel ----

#[test]
fn applies_fuzzy_matching_to_category_default_model_when_user_model_not_provided() {
    let _log = LogCapture::install();
    let input = ExtendedModelResolutionInput {
        category_default_model: some("google/gemini-3.1-pro"),
        fallback_chain: chain(&[(&["google", "github-copilot", "opencode"], "gemini-3.1-pro")]),
        available_models: set(&["google/gemini-3.1-pro-preview", "anthropic/claude-opus-4-7"]),
        system_default_model: some("anthropic/claude-sonnet-4-6"),
        ..Default::default()
    };

    let result = resolved(&input);

    assert_eq!(result.model, "google/gemini-3.1-pro-preview");
    assert_eq!(result.source, ModelResolutionProvenance::CategoryDefault);
}

#[test]
fn category_default_model_uses_exact_match_when_available() {
    let _log = LogCapture::install();
    let input = ExtendedModelResolutionInput {
        category_default_model: some("google/gemini-3.1-pro"),
        fallback_chain: chain(&[(&["google"], "gemini-3.1-pro")]),
        available_models: set(&["google/gemini-3.1-pro", "google/gemini-3.1-pro-preview"]),
        system_default_model: some("anthropic/claude-sonnet-4-6"),
        ..Default::default()
    };

    let result = resolved(&input);

    assert_eq!(result.model, "google/gemini-3.1-pro");
    assert_eq!(result.source, ModelResolutionProvenance::CategoryDefault);
}

#[test]
fn category_default_model_falls_through_to_fallback_chain_when_no_match_in_available_models() {
    let _log = LogCapture::install();
    let input = ExtendedModelResolutionInput {
        category_default_model: some("google/gemini-3.1-pro"),
        fallback_chain: chain(&[(&["anthropic"], "claude-opus-4-7")]),
        available_models: set(&["anthropic/claude-opus-4-7"]),
        system_default_model: some("system/default"),
        ..Default::default()
    };

    assert_provider_fallback(&resolved(&input), "anthropic/claude-opus-4-7");
}

#[test]
fn user_model_takes_priority_over_category_default_model() {
    let _log = LogCapture::install();
    let input = ExtendedModelResolutionInput {
        user_model: some("anthropic/claude-opus-4-7"),
        category_default_model: some("google/gemini-3.1-pro"),
        fallback_chain: chain(&[(&["google"], "gemini-3.1-pro")]),
        available_models: set(&["google/gemini-3.1-pro-preview", "anthropic/claude-opus-4-7"]),
        system_default_model: some("system/default"),
        ..Default::default()
    };

    let result = resolved(&input);

    assert_eq!(result.model, "anthropic/claude-opus-4-7");
    assert_eq!(result.source, ModelResolutionProvenance::Override);
}

fn connected_category_default(
    category_default: &str,
    system_default: &str,
) -> ModelResolutionResult {
    let input = ExtendedModelResolutionInput {
        category_default_model: some(category_default),
        system_default_model: some(system_default),
        ..Default::default()
    };
    resolved_with(&input, &ConnectedCache::with(&["google"]))
}

fn assert_category_default(result: &ModelResolutionResult, model: &str) {
    assert_eq!(result.model, model);
    assert_eq!(result.source, ModelResolutionProvenance::CategoryDefault);
}

#[test]
fn category_default_model_works_when_available_models_is_empty_but_connected_provider_exists() {
    let _log = LogCapture::install();
    assert_category_default(
        &connected_category_default("google/gemini-3.1-pro", "anthropic/claude-sonnet-4-6"),
        "google/gemini-3.1-pro-preview",
    );
}

#[test]
fn transforms_gemini_3_flash_in_category_default_model_for_google_connected_provider() {
    let _log = LogCapture::install();
    assert_category_default(
        &connected_category_default("google/gemini-3-flash", "anthropic/claude-sonnet-4-5"),
        "google/gemini-3-flash-preview",
    );
}

#[test]
fn does_not_double_transform_category_default_model_already_containing_preview() {
    let _log = LogCapture::install();
    assert_category_default(
        &connected_category_default(
            "google/gemini-3.1-pro-preview",
            "anthropic/claude-sonnet-4-5",
        ),
        "google/gemini-3.1-pro-preview",
    );
}

#[test]
fn transforms_gemini_3_1_pro_in_fallback_chain_for_google_connected_provider() {
    let _log = LogCapture::install();
    let input = chain_input(
        &[(&["google", "github-copilot"], "gemini-3.1-pro")],
        &[],
        Some("anthropic/claude-sonnet-4-5"),
    );

    assert_provider_fallback(
        &resolved_with(&input, &ConnectedCache::with(&["google"])),
        "google/gemini-3.1-pro-preview",
    );
}

#[test]
fn passes_through_non_gemini_3_models_for_google_connected_provider() {
    let _log = LogCapture::install();
    assert_category_default(
        &connected_category_default("google/gemini-2.5-flash", "anthropic/claude-sonnet-4-5"),
        "google/gemini-2.5-flash",
    );
}

// ---- Optional systemDefaultModel ----

#[test]
fn returns_none_when_system_default_model_is_undefined_and_no_fallback_found() {
    let _log = LogCapture::install();
    let input = chain_input(
        &[(&["anthropic"], "nonexistent-model")],
        &["openai/gpt-5.4"],
        None,
    );

    assert_eq!(resolve_model_with_fallback(&input), None);
}

#[test]
fn returns_none_when_no_fallback_chain_and_system_default_model_is_undefined() {
    let _log = LogCapture::install();
    let input = ExtendedModelResolutionInput {
        available_models: set(&["openai/gpt-5.4"]),
        ..Default::default()
    };

    assert_eq!(resolve_model_with_fallback(&input), None);
}

#[test]
fn still_returns_override_when_user_model_provided_even_if_system_default_model_undefined() {
    let _log = LogCapture::install();
    let input = ExtendedModelResolutionInput {
        user_model: some("anthropic/claude-opus-4-7"),
        ..Default::default()
    };

    let result = resolved(&input);

    assert_eq!(result.model, "anthropic/claude-opus-4-7");
    assert_eq!(result.source, ModelResolutionProvenance::Override);
}

#[test]
fn still_returns_fallback_match_when_system_default_model_undefined() {
    let _log = LogCapture::install();
    let input = chain_input(
        &[(&["anthropic"], "claude-opus-4-7")],
        &["anthropic/claude-opus-4-7"],
        None,
    );

    assert_provider_fallback(&resolved(&input), "anthropic/claude-opus-4-7");
}
