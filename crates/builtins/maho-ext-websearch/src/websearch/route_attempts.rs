//! Port of senpi `websearch/websearch/route-attempts.ts` (pin 903b8336).
use super::types::SearchProviderEntry;

/// `providerEntryLabel`: the provider, or `provider/id`, with discovered native ids shown as `native`.
pub fn provider_entry_label(provider: &str, id: Option<&str>, entry_id: Option<&str>) -> String {
    let Some(id) = entry_id
        .or(id)
        .filter(|id| !id.is_empty() && *id != provider)
    else {
        return provider.into();
    };
    if id.ends_with("/native") {
        return id.into();
    }
    let native_prefix = format!("native-{provider}-");
    format!(
        "{provider}/{}",
        if id.starts_with(&native_prefix) {
            "native"
        } else {
            id
        }
    )
}

/// `attemptRouteLabel`: the entry label with its model in parentheses.
pub fn attempt_route_label(
    provider: &str,
    id: Option<&str>,
    entry_id: Option<&str>,
    model: Option<&str>,
) -> String {
    let label = provider_entry_label(provider, id, entry_id);
    match model.filter(|model| !model.is_empty()) {
        Some(model) => format!("{label} ({model})"),
        None => label,
    }
}

/// `routeAttemptEntries`: the entry, then the same entry on its `fallback_model` when that differs.
pub fn route_attempt_entries(entry: &SearchProviderEntry) -> Vec<SearchProviderEntry> {
    let mut primary = entry.clone();
    let fallback_model = primary.fallback_model.take();
    match fallback_model.filter(|model| {
        !model.is_empty() && Some(model.as_str()) != primary.config.model.as_deref()
    }) {
        None => vec![primary],
        Some(model) => {
            let mut fallback = primary.clone();
            fallback.config.model = Some(model);
            vec![primary, fallback]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::websearch::types::{SearchProvider, SearchProviderConfig};

    #[test]
    fn labels_entries_and_discovered_native_routes() {
        assert_eq!(provider_entry_label("exa", None, None), "exa");
        assert_eq!(provider_entry_label("exa", Some("exa"), None), "exa");
        assert_eq!(provider_entry_label("exa", Some("work"), None), "exa/work");
        assert_eq!(
            provider_entry_label("exa", Some("work"), Some("other")),
            "exa/other"
        );
        assert_eq!(
            provider_entry_label("openai", Some("native-openai-0123456789abcdef"), None),
            "openai/native"
        );
        assert_eq!(
            provider_entry_label("openai", Some("openrouter/native"), None),
            "openrouter/native"
        );
        assert_eq!(
            attempt_route_label("openai", Some("work"), None, Some("gpt-5")),
            "openai/work (gpt-5)"
        );
        assert_eq!(attempt_route_label("openai", None, None, None), "openai");
    }

    #[test]
    fn adds_a_fallback_model_attempt_only_when_it_differs() {
        let mut config = SearchProviderConfig::new(SearchProvider::Anthropic);
        config.model = Some("claude-haiku-4-5".into());
        let mut entry = SearchProviderEntry {
            config,
            priority: None,
            weight: None,
            fallback_model: None,
        };
        assert_eq!(route_attempt_entries(&entry).len(), 1);
        entry.fallback_model = Some("claude-haiku-4-5".into());
        assert_eq!(route_attempt_entries(&entry).len(), 1);
        entry.fallback_model = Some("claude-opus-5-5".into());
        let attempts = route_attempt_entries(&entry);
        assert!(
            attempts
                .iter()
                .all(|attempt| attempt.fallback_model.is_none())
        );
        assert_eq!(attempts.len(), 2);
        assert_eq!(
            attempts[0].config.model.as_deref(),
            Some("claude-haiku-4-5")
        );
        assert_eq!(attempts[1].config.model.as_deref(), Some("claude-opus-5-5"));
    }
}
