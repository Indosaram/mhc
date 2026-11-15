//! Port of senpi `websearch/websearch/providers/openrouter.ts`.
use crate::websearch::providers::shared::{
    BuildContext, BuiltSearchRequest, SearchResultItem, clamp, collect, content_headers, result,
    unique_by_url,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub const DEFAULT_URL: &str = "https://openrouter.ai/api/v1/chat/completions";

fn chat_completions_url<'a>(base_url: Option<&'a str>) -> std::borrow::Cow<'a, str> {
    let root = base_url.unwrap_or(DEFAULT_URL).trim_end_matches('/');
    if root.ends_with("/chat/completions") {
        root.into()
    } else {
        format!("{root}/chat/completions").into()
    }
}

fn http_url(value: Option<&str>) -> Option<&str> {
    let url = value.filter(|url| !url.is_empty())?;
    let parsed = url::Url::parse(url).ok()?;
    matches!(parsed.scheme(), "http" | "https").then_some(url)
}

pub fn build_request(ctx: &BuildContext<'_>, model: Option<&str>) -> BuiltSearchRequest {
    let mut body = json!({
        "plugins": [{"id": "web", "max_results": clamp(ctx.max_results, 1.0, 50.0)}],
        "messages": [{"role": "user", "content": ctx.query}],
    });
    if let Some(model) = model {
        body["model"] = json!(model);
    }
    BuiltSearchRequest {
        form: None,
        url: chat_completions_url(ctx.base_url).into_owned(),
        method: "POST",
        headers: content_headers(BTreeMap::from([(
            "Authorization".into(),
            format!("Bearer {}", ctx.api_key.unwrap_or_default()),
        )])),
        body: Some(body),
    }
}

pub fn normalize_response(data: &Value) -> Vec<SearchResultItem> {
    let annotations = data["choices"][0]["message"]["annotations"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    unique_by_url(collect(annotations.iter().map(|annotation| {
        let kind = annotation["type"].as_str();
        if kind.is_some_and(|kind| kind != "url_citation") {
            return None;
        }
        let citation = &annotation["url_citation"];
        let url = http_url(citation["url"].as_str())?;
        result(
            citation["title"].as_str().or(Some(url)),
            Some(url),
            citation["content"].as_str(),
            None,
            None,
        )
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx<'a>(base: Option<&'a str>, max: f64) -> BuildContext<'a> {
        BuildContext {
            query: "who won",
            max_results: max,
            api_key: Some("or-key"),
            base_url: base,
            allowed_domains: None,
            blocked_domains: None,
        }
    }

    #[test]
    fn appends_the_chat_completions_path_once() {
        assert_eq!(
            build_request(&ctx(None, 5.0), Some("anthropic/claude")).url,
            DEFAULT_URL
        );
        assert_eq!(
            build_request(&ctx(Some("https://openrouter.ai/api/v1"), 5.0), None).url,
            DEFAULT_URL
        );
        assert_eq!(
            build_request(&ctx(Some(DEFAULT_URL), 5.0), None).url,
            DEFAULT_URL
        );
        assert_eq!(
            build_request(&ctx(Some("https://proxy.test/v1/"), 5.0), None).url,
            "https://proxy.test/v1/chat/completions"
        );
    }

    #[test]
    fn sends_the_web_plugin_with_a_clamped_count_and_an_optional_model() {
        let request = build_request(&ctx(None, 99.0), Some("openai/gpt-5"));
        assert_eq!(request.method, "POST");
        assert_eq!(request.headers["Authorization"], "Bearer or-key");
        assert_eq!(
            request.body.unwrap(),
            json!({
                "plugins": [{"id": "web", "max_results": 50.0}],
                "messages": [{"role": "user", "content": "who won"}],
                "model": "openai/gpt-5",
            })
        );
        assert!(build_request(&ctx(None, 3.0), None).body.unwrap()["model"].is_null());
    }

    #[test]
    fn keeps_only_unique_url_citation_annotations() {
        let results = normalize_response(&json!({"choices":[{"message":{"annotations":[
            {"type":"url_citation","url_citation":{"url":"https://a.test","title":"A","content":"aa"}},
            {"type":"url_citation","url_citation":{"url":"https://a.test","title":"dup"}},
            {"type":"url_citation","url_citation":{"url":"https://b.test"}},
            {"type":"other","url_citation":{"url":"https://c.test"}},
            {"type":"url_citation","url_citation":{"url":"not a url"}}
        ]}}]}));
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].title, "A");
        assert_eq!(results[0].snippet.as_deref(), Some("aa"));
        assert_eq!(results[1].title, "https://b.test");
    }

    #[test]
    fn an_answer_without_annotations_is_no_results() {
        assert!(
            normalize_response(&json!({"choices":[{"message":{"content":"text"}}]})).is_empty()
        );
        assert!(normalize_response(&json!({"choices":[]})).is_empty());
    }
}
