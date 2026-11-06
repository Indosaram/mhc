use super::shared::{
    BuildContext, BuiltSearchRequest, SearchResultItem, append_domain_filters, content_headers,
    result,
};
use crate::websearch::provider_endpoints::{SearchProvider, provider_url};
use serde_json::{Value, json};
use std::collections::BTreeMap;
pub fn build_request(
    ctx: &BuildContext<'_>,
    provider: SearchProvider,
    model: Option<&str>,
    codex_mode: Option<&str>,
    search_context_size: Option<&str>,
    user_location: Option<&Value>,
) -> BuiltSearchRequest {
    let mut tool =
        json!({"type":"web_search","external_web_access":codex_mode.unwrap_or("live")=="live"});
    if let Some(size) = search_context_size.filter(|size| !size.is_empty()) {
        tool["search_context_size"] = json!(size);
    }
    if let Some(domains) = ctx.allowed_domains {
        tool["filters"] = json!({"allowed_domains":domains});
    }
    if let Some(location) = user_location {
        let mut location = location.as_object().cloned().unwrap_or_default();
        location.entry("type").or_insert(json!("approximate"));
        tool["user_location"] = Value::Object(location);
    }
    let query = append_domain_filters(ctx.query, None, ctx.blocked_domains);
    let input = format!(
        "Find web pages matching any of these search terms or quoted phrases. If the query contains OR, search each alternative independently. Return only relevant source URLs, one per line. Query: {query}"
    );
    BuiltSearchRequest {
        form: None,
        url: provider_url(provider, ctx.base_url).into(),
        method: "POST",
        headers: content_headers(BTreeMap::from([(
            "Authorization".into(),
            format!("Bearer {}", ctx.api_key.unwrap_or_default()),
        )])),
        body: Some(
            json!({"model":model.unwrap_or("gpt-5.5"),"input":input,"tools":[tool],"include":["web_search_call.action.sources"],"tool_choice":"required"}),
        ),
    }
}
/// Only search output is a source: URLs the model wrote in its answer text were never returned by a
/// search and may be invented (senpi#2337).
pub fn normalize_responses_payload(
    data: &Value,
    citations_fallback: bool,
) -> Vec<SearchResultItem> {
    let output = data
        .get("output")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let mut sources: Vec<_> = output
        .iter()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("web_search_call"))
        .flat_map(|item| {
            item.get("action")
                .and_then(|action| action.get("sources"))
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
        })
        .filter_map(|item| {
            let url = item.get("url").and_then(Value::as_str);
            result(url, url, None, None, None)
        })
        .take(50)
        .collect();
    let content = output
        .iter()
        .find(|item| item.get("type").and_then(Value::as_str) == Some("message"))
        .and_then(|item| item.get("content"))
        .and_then(Value::as_array)
        .and_then(|items| {
            items
                .iter()
                .find(|item| item.get("type").and_then(Value::as_str) == Some("output_text"))
        });
    let text = content
        .and_then(|item| item.get("text"))
        .and_then(Value::as_str);
    let annotations: Vec<_> = content
        .and_then(|item| item.get("annotations"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("url_citation"))
        .filter_map(|item| {
            result(
                item.get("title").and_then(Value::as_str),
                item.get("url").and_then(Value::as_str),
                text,
                None,
                None,
            )
        })
        .take(50)
        .collect();
    if !annotations.is_empty() {
        return annotations;
    }
    if !sources.is_empty() {
        for source in &mut sources {
            source.snippet = text.map(str::to_owned);
        }
        return sources;
    }
    if !citations_fallback {
        return annotations;
    }
    data.get("citations")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|url| result(url.as_str(), url.as_str(), text, None, None))
        .take(50)
        .collect()
}
pub fn normalize_response(data: &Value) -> Vec<SearchResultItem> {
    normalize_responses_payload(data, false)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn annotation_precedes_sources() {
        let results = normalize_response(
            &json!({"output":[{"type":"web_search_call","action":{"sources":[{"url":"https://source"}]}},{"type":"message","content":[{"type":"output_text","text":"answer","annotations":[{"type":"url_citation","title":"citation","url":"https://citation"}]}]}]}),
        );
        assert_eq!(results[0].title, "citation");
        assert_eq!(results[0].snippet.as_deref(), Some("answer"));
    }
    fn answer_only() -> Value {
        json!({"output":[{"type":"message","content":[{"type":"output_text","text":"See https://made-up.example.com/page for details.","annotations":[]}]}]})
    }
    #[test]
    fn answer_text_urls_are_not_sources_issue_2337() {
        assert!(normalize_response(&answer_only()).is_empty());
        assert!(super::super::xai::normalize_response(&answer_only()).is_empty());
        let mut found = answer_only();
        found["output"].as_array_mut().unwrap().insert(0,json!({"type":"web_search_call","action":{"sources":[{"url":"https://found.example.com/a"}]}}));
        assert_eq!(
            normalize_response(&found)
                .iter()
                .map(|item| item.url.as_str())
                .collect::<Vec<_>>(),
            ["https://found.example.com/a"]
        );
        let mut cited = answer_only();
        cited["citations"] = json!(["https://cited.example.com/x"]);
        assert_eq!(
            super::super::xai::normalize_response(&cited)
                .iter()
                .map(|item| item.url.as_str())
                .collect::<Vec<_>>(),
            ["https://cited.example.com/x"]
        );
    }
    #[test]
    fn citations_are_optional() {
        let data = json!({"citations":["https://fallback"]});
        assert!(normalize_response(&data).is_empty());
        assert_eq!(normalize_responses_payload(&data, true).len(), 1);
    }
    #[test]
    fn cached_mode_and_empty_allowed_filters() {
        let ctx = BuildContext {
            query: "q",
            max_results: 1.,
            api_key: None,
            base_url: None,
            allowed_domains: Some(&[]),
            blocked_domains: None,
        };
        let req = build_request(
            &ctx,
            SearchProvider::Codex,
            None,
            Some("cached"),
            None,
            None,
        );
        assert_eq!(
            req.body.as_ref().expect("request body")["tools"][0]["external_web_access"],
            false
        );
        assert_eq!(
            req.body.as_ref().expect("request body")["tools"][0]["filters"]["allowed_domains"],
            json!([])
        );
    }
}
