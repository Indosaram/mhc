//! Port of senpi `websearch/websearch/hosted-routes.ts` (pin 903b8336).
use std::collections::BTreeMap;

/// The search provider a hosted route maps to, by wire id (`native_mapping` resolves it to
/// `SearchProvider::ChatgptSubscription`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostedRouteMapping {
    pub provider: &'static str,
    pub resource: &'static str,
    pub route_label: Option<String>,
}

impl HostedRouteMapping {
    /// The endpoint the route searches through for a given model base URL.
    pub fn endpoint(&self, base_url: &str) -> String {
        subscription_responses_url(base_url)
    }
}

pub struct HostedModelInfo<'a> {
    pub provider: &'a str,
    pub id: &'a str,
    pub api: Option<&'a str>,
}

fn trim_trailing_slashes(base_url: &str) -> &str {
    base_url.trim_end_matches('/')
}

fn subscription_responses_url(base_url: &str) -> String {
    let normalized = trim_trailing_slashes(base_url);
    if normalized.ends_with("/codex/responses") {
        normalized.into()
    } else if normalized.ends_with("/codex") {
        format!("{normalized}/responses")
    } else {
        format!("{normalized}/codex/responses")
    }
}

fn is_google_search_model(id: &str) -> bool {
    id.starts_with("gemini-")
        && !["image", "live", "tts", "embedding", "computer-use"]
            .iter()
            .any(|word| id.contains(word))
}

/// Routes added automatically for the session's own provider. Google Search grounding is opt-in
/// and never listed here.
pub fn hosted_route_mapping(model: &HostedModelInfo<'_>) -> Option<HostedRouteMapping> {
    if model.api != Some("openai-codex-responses") || model.id.contains("-spark") {
        return None;
    }
    Some(HostedRouteMapping {
        provider: "chatgpt-subscription",
        resource: "codex/responses",
        route_label: (model.provider != "chatgpt-subscription").then(|| model.provider.to_owned()),
    })
}

/// The Google API root a listed `google` entry searches through with the `google` login.
pub fn google_login_endpoint(model: &HostedModelInfo<'_>, base_url: &str) -> Option<String> {
    if model.provider != "google"
        || model.api != Some("google-generative-ai")
        || !is_google_search_model(model.id)
    {
        return None;
    }
    Some(trim_trailing_slashes(base_url).into())
}

/// Catalog headers plus the credential's headers; a `None` credential header removes the catalog
/// one. Header names compare case-insensitively and are emitted lowercase, as `Headers` does.
pub fn credential_headers(
    model_headers: Option<&BTreeMap<String, String>>,
    auth_headers: Option<&BTreeMap<String, Option<String>>>,
) -> Option<BTreeMap<String, String>> {
    let mut headers: BTreeMap<String, String> = model_headers
        .into_iter()
        .flatten()
        .map(|(name, value)| (name.to_lowercase(), value.clone()))
        .collect();
    for (name, value) in auth_headers.into_iter().flatten() {
        match value {
            None => {
                headers.remove(&name.to_lowercase());
            }
            Some(value) => {
                headers.insert(name.to_lowercase(), value.clone());
            }
        }
    }
    (!headers.is_empty()).then_some(headers)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info<'a>(provider: &'a str, id: &'a str, api: &'a str) -> HostedModelInfo<'a> {
        HostedModelInfo {
            provider,
            id,
            api: Some(api),
        }
    }

    #[test]
    fn maps_codex_responses_to_the_subscription_route() {
        let mapping = hosted_route_mapping(&info(
            "chatgpt-subscription",
            "gpt-5.5",
            "openai-codex-responses",
        ))
        .unwrap();
        assert_eq!(mapping.provider, "chatgpt-subscription");
        assert_eq!(mapping.route_label, None);
        assert_eq!(
            mapping.endpoint("https://chatgpt.com/backend-api/"),
            "https://chatgpt.com/backend-api/codex/responses"
        );
        assert_eq!(
            mapping.endpoint("https://x/codex"),
            "https://x/codex/responses"
        );
        assert_eq!(
            mapping.endpoint("https://x/codex/responses"),
            "https://x/codex/responses"
        );
        let proxied =
            hosted_route_mapping(&info("proxy", "gpt-5.5", "openai-codex-responses")).unwrap();
        assert_eq!(proxied.route_label.as_deref(), Some("proxy"));
        assert!(
            hosted_route_mapping(&info(
                "chatgpt-subscription",
                "gpt-5.5-spark",
                "openai-codex-responses"
            ))
            .is_none()
        );
        assert!(hosted_route_mapping(&info("openai", "gpt-5.5", "openai-responses")).is_none());
    }

    #[test]
    fn google_login_endpoint_only_for_search_capable_gemini() {
        let model = info("google", "gemini-3-pro", "google-generative-ai");
        assert_eq!(
            google_login_endpoint(&model, "https://g.example/v1beta//").as_deref(),
            Some("https://g.example/v1beta")
        );
        assert!(
            google_login_endpoint(
                &info("google", "gemini-2.5-flash-image", "google-generative-ai"),
                "https://g"
            )
            .is_none()
        );
        assert!(
            google_login_endpoint(
                &info("vertex", "gemini-3-pro", "google-generative-ai"),
                "https://g"
            )
            .is_none()
        );
    }

    #[test]
    fn credential_headers_overlay_and_remove() {
        let model = BTreeMap::from([
            ("X-Catalog".to_string(), "c".to_string()),
            ("X-Drop".to_string(), "d".to_string()),
        ]);
        let auth = BTreeMap::from([
            ("x-drop".to_string(), None),
            ("Authorization".to_string(), Some("Bearer k".to_string())),
        ]);
        let merged = credential_headers(Some(&model), Some(&auth)).unwrap();
        assert_eq!(merged.get("x-catalog").map(String::as_str), Some("c"));
        assert_eq!(
            merged.get("authorization").map(String::as_str),
            Some("Bearer k")
        );
        assert!(!merged.contains_key("x-drop"));
        assert_eq!(credential_headers(None, None), None);
    }
}
