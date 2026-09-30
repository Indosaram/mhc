//! Port of senpi packages/ai/src/legacy-provider-ids.ts.

use std::collections::BTreeMap;

pub const LEGACY_PROVIDER_IDS: &[(&str, &str)] =
    &[("openai-codex", "chatgpt-subscription"), ("claude-sdk-oauth", "anthropic-subscription")];

const LEGACY_PROVIDER_DISPLAY_NAMES: &[(&str, &str)] =
    &[("openai codex", "openai-codex"), ("claude sdk oauth", "claude-sdk-oauth")];

fn canonical_for(id: &str) -> Option<&'static str> {
    LEGACY_PROVIDER_IDS.iter().find(|(legacy, _)| *legacy == id).map(|(_, canonical)| *canonical)
}

pub fn normalize_provider_id(id: &str) -> String {
    canonical_for(id).unwrap_or(id).to_owned()
}

pub fn normalize_model_ref(model_ref: &str) -> String {
    let Some((provider, model)) = model_ref.split_once('/') else {
        return normalize_provider_id(model_ref);
    };
    match canonical_for(provider) {
        Some(canonical) => format!("{canonical}/{model}"),
        None => model_ref.to_owned(),
    }
}

pub fn is_legacy_provider_id(id: &str) -> bool {
    canonical_for(id).is_some()
}

/// Legacy spellings that normalize to `canonical_id`.
pub fn legacy_provider_ids_for(canonical_id: &str) -> Vec<&'static str> {
    LEGACY_PROVIDER_IDS.iter().filter(|(_, canonical)| *canonical == canonical_id).map(|(legacy, _)| *legacy).collect()
}

/// Reads a record keyed by provider id, preferring the canonical key over legacy spellings.
pub fn read_by_provider_id<'a, T>(record: Option<&'a BTreeMap<String, T>>, provider_id: &str) -> Option<&'a T> {
    let record = record?;
    let canonical = normalize_provider_id(provider_id);
    record
        .get(&canonical)
        .or_else(|| legacy_provider_ids_for(&canonical).into_iter().find_map(|legacy| record.get(legacy)))
}

/// Rejection text for a typed legacy provider id or display name.
pub fn legacy_provider_id_rejection(typed: &str) -> Option<String> {
    let lowered = crate::utils::js::trim(typed).to_lowercase();
    let legacy_id = LEGACY_PROVIDER_DISPLAY_NAMES
        .iter()
        .find(|(display, _)| *display == lowered)
        .map(|(_, id)| *id)
        .or_else(|| LEGACY_PROVIDER_IDS.iter().find(|(legacy, _)| *legacy == lowered).map(|(legacy, _)| *legacy))?;
    let canonical = canonical_for(legacy_id)?;
    Some(format!("{legacy_id} was renamed to {canonical}. Use {canonical}."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_openai_codex_to_chatgpt_subscription() {
        assert_eq!(normalize_provider_id("openai-codex"), "chatgpt-subscription");
    }

    #[test]
    fn maps_claude_sdk_oauth_to_anthropic_subscription() {
        assert_eq!(normalize_provider_id("claude-sdk-oauth"), "anthropic-subscription");
    }

    #[test]
    fn leaves_anthropic_unchanged() {
        assert_eq!(normalize_provider_id("anthropic"), "anthropic");
    }

    #[test]
    fn leaves_openai_unchanged() {
        assert_eq!(normalize_provider_id("openai"), "openai");
    }

    #[test]
    fn is_idempotent_for_legacy_and_canonical_ids() {
        for id in ["openai-codex", "claude-sdk-oauth", "chatgpt-subscription", "anthropic-subscription", "anthropic"] {
            let once = normalize_provider_id(id);
            assert_eq!(normalize_provider_id(&once), once);
        }
    }

    #[test]
    fn normalizes_the_provider_half_of_a_model_ref() {
        assert_eq!(normalize_model_ref("openai-codex/gpt-5.5"), "chatgpt-subscription/gpt-5.5");
        assert_eq!(normalize_model_ref("claude-sdk-oauth/claude-opus-4-8"), "anthropic-subscription/claude-opus-4-8");
        assert_eq!(normalize_model_ref("anthropic/claude-opus-4-8"), "anthropic/claude-opus-4-8");
    }

    #[test]
    fn keeps_a_model_id_containing_a_slash_intact() {
        assert_eq!(normalize_model_ref("openai-codex/org/model"), "chatgpt-subscription/org/model");
    }

    #[test]
    fn handles_empty_and_slash_free_refs_without_throwing() {
        assert_eq!(normalize_model_ref(""), "");
        assert_eq!(normalize_model_ref("openai-codex"), "chatgpt-subscription");
        assert_eq!(normalize_model_ref("gpt-5.5"), "gpt-5.5");
    }

    #[test]
    fn detects_only_the_legacy_provider_ids() {
        assert!(is_legacy_provider_id("openai-codex"));
        assert!(is_legacy_provider_id("claude-sdk-oauth"));
        assert!(!is_legacy_provider_id("chatgpt-subscription"));
        assert!(!is_legacy_provider_id("anthropic"));
    }

    fn record(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()
    }

    #[test]
    fn maps_a_canonical_id_back_to_its_legacy_spellings() {
        assert_eq!(legacy_provider_ids_for("chatgpt-subscription"), vec!["openai-codex"]);
        assert_eq!(legacy_provider_ids_for("anthropic-subscription"), vec!["claude-sdk-oauth"]);
    }

    #[test]
    fn returns_no_legacy_spelling_for_an_id_that_was_never_renamed() {
        assert!(legacy_provider_ids_for("anthropic").is_empty());
        assert!(legacy_provider_ids_for("openai").is_empty());
    }

    #[test]
    fn reads_a_record_written_under_the_legacy_key_by_its_canonical_id() {
        let r = record(&[("openai-codex", "legacy")]);
        assert_eq!(read_by_provider_id(Some(&r), "chatgpt-subscription").map(String::as_str), Some("legacy"));
        assert_eq!(read_by_provider_id(Some(&r), "openai-codex").map(String::as_str), Some("legacy"));
    }

    #[test]
    fn prefers_the_canonical_key_when_both_spellings_are_present() {
        let r = record(&[("openai-codex", "legacy"), ("chatgpt-subscription", "canonical")]);
        assert_eq!(read_by_provider_id(Some(&r), "chatgpt-subscription").map(String::as_str), Some("canonical"));
        assert_eq!(read_by_provider_id(Some(&r), "openai-codex").map(String::as_str), Some("canonical"));
    }

    #[test]
    fn never_confuses_the_untouched_api_key_providers() {
        let r = record(&[("anthropic", "api-key"), ("anthropic-subscription", "subscription")]);
        assert_eq!(read_by_provider_id(Some(&r), "anthropic").map(String::as_str), Some("api-key"));
        assert_eq!(read_by_provider_id(Some(&r), "anthropic-subscription").map(String::as_str), Some("subscription"));
    }

    #[test]
    fn returns_undefined_for_a_missing_id_and_an_absent_record() {
        let r = record(&[("x", "1")]);
        assert_eq!(read_by_provider_id(Some(&r), "anthropic-subscription"), None);
        assert_eq!(read_by_provider_id::<String>(None, "anthropic-subscription"), None);
    }

    #[test]
    fn rejects_typed_legacy_ids_and_display_names() {
        assert_eq!(
            legacy_provider_id_rejection("  OpenAI Codex ").as_deref(),
            Some("openai-codex was renamed to chatgpt-subscription. Use chatgpt-subscription.")
        );
        assert_eq!(
            legacy_provider_id_rejection("claude-sdk-oauth").as_deref(),
            Some("claude-sdk-oauth was renamed to anthropic-subscription. Use anthropic-subscription.")
        );
        assert_eq!(legacy_provider_id_rejection("anthropic"), None);
    }
}
