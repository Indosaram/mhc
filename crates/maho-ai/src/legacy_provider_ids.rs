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

    fn record(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()
    }

    // senpi test/legacy-provider-ids.test.ts (9 cases). Each row's first field is the TS `it()`
    // title verbatim.

    #[test]
    fn maps_openai_codex_to_chatgpt_subscription() {
        let title = "maps openai-codex to chatgpt-subscription";
        assert_eq!(normalize_provider_id("openai-codex"), "chatgpt-subscription", "{title}");
    }

    #[test]
    fn maps_claude_sdk_oauth_to_anthropic_subscription() {
        let title = "maps claude-sdk-oauth to anthropic-subscription";
        assert_eq!(normalize_provider_id("claude-sdk-oauth"), "anthropic-subscription", "{title}");
    }

    #[test]
    fn leaves_anthropic_unchanged() {
        let title = "leaves anthropic unchanged";
        assert_eq!(normalize_provider_id("anthropic"), "anthropic", "{title}");
    }

    #[test]
    fn leaves_openai_unchanged() {
        let title = "leaves openai unchanged";
        assert_eq!(normalize_provider_id("openai"), "openai", "{title}");
    }

    #[test]
    fn is_idempotent_for_legacy_and_canonical_ids() {
        let title = "is idempotent for legacy and canonical ids";
        for id in ["openai-codex", "claude-sdk-oauth", "anthropic"] {
            let once = normalize_provider_id(id);
            assert_eq!(normalize_provider_id(&once), once, "{title} ({id})");
        }
    }

    #[test]
    fn normalizes_the_provider_half_of_a_model_ref() {
        let title = "normalizes the provider half of a model ref";
        assert_eq!(normalize_model_ref("openai-codex/gpt-5.6-sol"), "chatgpt-subscription/gpt-5.6-sol", "{title}");
    }

    #[test]
    fn keeps_a_model_id_containing_a_slash_intact() {
        let title = "keeps a model id containing a slash intact";
        assert_eq!(normalize_model_ref("openai-codex/vendor/model-x"), "chatgpt-subscription/vendor/model-x", "{title}");
    }

    #[test]
    fn handles_empty_and_slash_free_refs_without_throwing() {
        let title = "handles empty and slash-free refs without throwing";
        assert_eq!(normalize_model_ref(""), "", "{title}");
        assert_eq!(normalize_model_ref("gpt-5.6-sol"), "gpt-5.6-sol", "{title}");
    }

    #[test]
    fn detects_only_the_legacy_provider_ids() {
        let title = "detects only the legacy provider ids";
        assert!(is_legacy_provider_id("openai-codex"), "{title}");
        assert!(is_legacy_provider_id("claude-sdk-oauth"), "{title}");
        assert!(!is_legacy_provider_id("anthropic"), "{title}");
        assert!(!is_legacy_provider_id("openai"), "{title}");
        assert!(!is_legacy_provider_id("unknown"), "{title}");
    }

    // senpi test/legacy-provider-id-read.test.ts (6 cases).

    #[test]
    fn maps_a_canonical_id_back_to_its_legacy_spellings() {
        let title = "maps a canonical id back to its legacy spellings";
        assert_eq!(legacy_provider_ids_for("anthropic-subscription"), vec!["claude-sdk-oauth"], "{title}");
        assert_eq!(legacy_provider_ids_for("chatgpt-subscription"), vec!["openai-codex"], "{title}");
    }

    #[test]
    fn returns_no_legacy_spelling_for_an_id_that_was_never_renamed() {
        let title = "returns no legacy spelling for an id that was never renamed";
        assert!(legacy_provider_ids_for("anthropic").is_empty(), "{title}");
        assert!(legacy_provider_ids_for("openai").is_empty(), "{title}");
    }

    #[test]
    fn reads_a_record_written_under_the_legacy_key_by_its_canonical_id() {
        let title = "reads a record written under the legacy key by its canonical id";
        let r = record(&[("claude-sdk-oauth", "legacy")]);
        assert_eq!(read_by_provider_id(Some(&r), "anthropic-subscription").map(String::as_str), Some("legacy"), "{title}");
        assert_eq!(read_by_provider_id(Some(&r), "claude-sdk-oauth").map(String::as_str), Some("legacy"), "{title}");
    }

    #[test]
    fn prefers_the_canonical_key_when_both_spellings_are_present() {
        let title = "prefers the canonical key when both spellings are present";
        let r = record(&[("anthropic-subscription", "canonical"), ("claude-sdk-oauth", "legacy")]);
        assert_eq!(read_by_provider_id(Some(&r), "anthropic-subscription").map(String::as_str), Some("canonical"), "{title}");
    }

    #[test]
    fn never_confuses_the_untouched_api_key_providers() {
        let title = "never confuses the untouched API-key providers";
        let r = record(&[("anthropic", "api-key"), ("anthropic-subscription", "subscription")]);
        assert_eq!(read_by_provider_id(Some(&r), "anthropic").map(String::as_str), Some("api-key"), "{title}");
        assert_eq!(read_by_provider_id(Some(&r), "anthropic-subscription").map(String::as_str), Some("subscription"), "{title}");
    }

    #[test]
    fn returns_undefined_for_a_missing_id_and_an_absent_record() {
        let title = "returns undefined for a missing id and an absent record";
        let r = record(&[("x", "1")]);
        assert_eq!(read_by_provider_id(Some(&r), "anthropic-subscription"), None, "{title}");
        assert_eq!(read_by_provider_id::<String>(None, "anthropic-subscription"), None, "{title}");
    }

    // senpi test/typed-legacy-provider-rejection.test.ts (5 cases).

    #[test]
    fn names_both_the_old_and_the_new_id_for_each_renamed_provider() {
        let title = "names both the old and the new id for each renamed provider";
        let codex = legacy_provider_id_rejection("openai-codex").expect("rejected");
        assert!(codex.contains("openai-codex"), "{title}");
        assert!(codex.contains("chatgpt-subscription"), "{title}");
        let claude = legacy_provider_id_rejection("claude-sdk-oauth").expect("rejected");
        assert!(claude.contains("claude-sdk-oauth"), "{title}");
        assert!(claude.contains("anthropic-subscription"), "{title}");
    }

    #[test]
    fn rejects_the_legacy_display_names_which_are_typed_surfaces_too() {
        let title = "rejects the legacy DISPLAY NAMES, which are typed surfaces too";
        assert!(
            legacy_provider_id_rejection("OpenAI Codex").is_some_and(|m| m.contains("chatgpt-subscription")),
            "{title}"
        );
        assert!(
            legacy_provider_id_rejection("Claude SDK OAuth").is_some_and(|m| m.contains("anthropic-subscription")),
            "{title}"
        );
    }

    #[test]
    fn is_case_and_whitespace_insensitive_the_way_a_typed_argument_is() {
        let title = "is case- and whitespace-insensitive the way a typed argument is";
        assert!(
            legacy_provider_id_rejection("  Openai-Codex  ").is_some_and(|m| m.contains("chatgpt-subscription")),
            "{title}"
        );
    }

    #[test]
    fn does_not_reject_the_canonical_ids() {
        let title = "does not reject the canonical ids";
        assert_eq!(legacy_provider_id_rejection("chatgpt-subscription"), None, "{title}");
        assert_eq!(legacy_provider_id_rejection("anthropic-subscription"), None, "{title}");
    }

    #[test]
    fn does_not_reject_the_untouched_api_key_providers_or_an_unrelated_id() {
        let title = "does not reject the untouched API-key providers or an unrelated id";
        assert_eq!(legacy_provider_id_rejection("anthropic"), None, "{title}");
        assert_eq!(legacy_provider_id_rejection("openai"), None, "{title}");
        assert_eq!(legacy_provider_id_rejection("google"), None, "{title}");
        assert_eq!(legacy_provider_id_rejection(""), None, "{title}");
    }
}
