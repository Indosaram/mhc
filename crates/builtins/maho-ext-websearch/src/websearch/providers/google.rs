use super::shared::{
    BuildContext, BuiltSearchRequest, SearchResultItem, append_domain_filters, content_headers,
    result,
};
use crate::websearch::provider_endpoints::{SearchProvider, provider_url};
use serde_json::{Value, json};
use std::collections::BTreeMap;
/// Catalog GA flash alias: it tracks the current stable flash model, so the default
/// cannot age into a retired generation (gemini-2.5-flash 404s for new accounts).
/// An entry-level `model` still overrides it.
pub const DEFAULT_MODEL: &str = "gemini-flash-latest";
fn generate_content_url(base_url: Option<&str>, model: Option<&str>) -> String {
    let root = provider_url(SearchProvider::Google, base_url).trim_end_matches('/');
    if root.ends_with(":generateContent") {
        return root.to_owned();
    }
    let model = model.unwrap_or(DEFAULT_MODEL);
    let model = model.strip_prefix("models/").unwrap_or(model);
    format!(
        "{root}/models/{}:generateContent",
        url::form_urlencoded::byte_serialize(model.as_bytes())
            .collect::<String>()
            .replace('+', "%20")
    )
}
pub fn build_request(ctx: &BuildContext<'_>, model: Option<&str>) -> BuiltSearchRequest {
    let query = append_domain_filters(ctx.query, ctx.allowed_domains, ctx.blocked_domains);
    let prompt = format!(
        "Search the web with Google Search and answer briefly, citing the pages you used. Query: {query}"
    );
    BuiltSearchRequest {
        form: None,
        url: generate_content_url(ctx.base_url, model),
        method: "POST",
        headers: content_headers(BTreeMap::from([(
            "x-goog-api-key".into(),
            ctx.api_key.unwrap_or_default().into(),
        )])),
        body: Some(
            json!({"contents":[{"role":"user","parts":[{"text":prompt}]}],"tools":[{"google_search":{}}]}),
        ),
    }
}
fn support_snippets(metadata: Option<&Value>) -> BTreeMap<u64, String> {
    let mut snippets = BTreeMap::new();
    for support in metadata
        .and_then(|metadata| metadata.get("groundingSupports"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(text) = support
            .get("segment")
            .and_then(|segment| segment.get("text"))
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
        else {
            continue;
        };
        for index in support
            .get("groundingChunkIndices")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_u64)
        {
            snippets.entry(index).or_insert_with(|| text.to_owned());
        }
    }
    snippets
}
/// Grounding chunks are the only results; an answer the model gave without grounding yields none.
pub fn normalize_response(data: &Value) -> Vec<SearchResultItem> {
    data.get("candidates")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .flat_map(|candidate| {
            let metadata = candidate.get("groundingMetadata");
            let snippets = support_snippets(metadata);
            metadata
                .and_then(|metadata| metadata.get("groundingChunks"))
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .enumerate()
                .filter_map(|(index, chunk)| {
                    let web = chunk.get("web");
                    let url = web.and_then(|web| web.get("uri")).and_then(Value::as_str);
                    result(
                        web.and_then(|web| web.get("title"))
                            .and_then(Value::as_str)
                            .or(url),
                        url,
                        snippets.get(&(index as u64)).map(String::as_str),
                        None,
                        None,
                    )
                })
                .collect::<Vec<_>>()
        })
        .take(50)
        .collect()
}
#[cfg(test)]
mod tests {
    use super::*;
    fn ctx<'a>(base_url: Option<&'a str>) -> BuildContext<'a> {
        BuildContext {
            query: "q",
            max_results: 5.,
            api_key: Some("k"),
            base_url,
            allowed_domains: None,
            blocked_domains: None,
        }
    }
    #[test]
    fn default_model_is_the_flash_alias() {
        assert_eq!(DEFAULT_MODEL, "gemini-flash-latest");
        assert_eq!(
            build_request(&ctx(None), None).url,
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-flash-latest:generateContent"
        );
    }
    #[test]
    fn entry_model_overrides_and_models_prefix_is_stripped() {
        assert!(
            build_request(&ctx(None), Some("models/gemini-3.5-flash"))
                .url
                .ends_with("/models/gemini-3.5-flash:generateContent")
        );
        assert_eq!(
            build_request(&ctx(Some("https://x.example/v1/m:generateContent")), None).url,
            "https://x.example/v1/m:generateContent"
        );
    }
    #[test]
    fn grounding_chunks_are_the_only_results() {
        let data = json!({"candidates":[{"groundingMetadata":{"groundingChunks":[{"web":{"uri":"https://a","title":"A"}},{"web":{"uri":"https://b"}}],"groundingSupports":[{"segment":{"text":"first"},"groundingChunkIndices":[0]},{"segment":{"text":"later"},"groundingChunkIndices":[0,1]}]}},{"content":"ungrounded"}]});
        let results = normalize_response(&data);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].snippet.as_deref(), Some("first"));
        assert_eq!(results[1].title, "https://b");
        assert_eq!(results[1].snippet.as_deref(), Some("later"));
        assert!(normalize_response(&json!({"candidates":[{"content":{}}]})).is_empty());
    }
}
