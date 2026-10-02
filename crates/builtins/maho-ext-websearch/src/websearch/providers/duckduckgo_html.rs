use std::{collections::BTreeMap,sync::LazyLock};
use regex::Regex;
use serde_json::Value;
use super::shared::{BuildContext,BuiltSearchRequest,SearchResultItem,append_domain_filters,result};
use crate::websearch::provider_endpoints::{provider_url,SearchProvider};
static LINKS:LazyLock<Regex>=LazyLock::new(||Regex::new(r#"<a\b[^>]*class="[^"]*result__a[^"]*"[^>]*href="([^"]+)"[^>]*>([\s\S]*?)</a>"#).expect("literal pattern"));
static SNIPPETS:LazyLock<Regex>=LazyLock::new(||Regex::new(r#"<a\b[^>]*class="[^"]*result__snippet[^"]*"[^>]*>([\s\S]*?)</a>"#).expect("literal pattern"));
static TAGS:LazyLock<Regex>=LazyLock::new(||Regex::new(r"<[^>]*>").expect("literal pattern"));
static SPACE:LazyLock<Regex>=LazyLock::new(||Regex::new(r"[\x09-\x0d\x20\x{a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}]+").expect("literal pattern"));
fn html_decode(value:&str)->String { value.replace("&amp;","&").replace("&quot;","\"").replace("&#39;","'").replace("&lt;","<").replace("&gt;",">") }
fn strip_html(value:&str)->String { html_decode(SPACE.replace_all(&TAGS.replace_all(value,"")," ").trim_matches(' ')) }
pub fn build_request(ctx:&BuildContext<'_>)->Result<BuiltSearchRequest,url::ParseError> {
    let mut url=url::Url::parse(provider_url(SearchProvider::DuckduckgoHtml,ctx.base_url))?;
    let mut replaced=false; let query=append_domain_filters(ctx.query,ctx.allowed_domains,ctx.blocked_domains);
    let mut pairs=Vec::new();
    for (key,value) in url.query_pairs() {
        if key=="q" { if !replaced { pairs.push((key.into_owned(),query.clone())); replaced=true; } }
        else { pairs.push((key.into_owned(),value.into_owned())); }
    }
    if !replaced { pairs.push(("q".into(),query)); }
    url.query_pairs_mut().clear().extend_pairs(pairs);
    Ok(BuiltSearchRequest{url:url.into(),method:"GET",headers:BTreeMap::from([("Accept".into(),"text/html".into())]),body:Value::Null})
}
pub fn normalize_response(data:&Value)->Vec<SearchResultItem> {
    let html=data.get("html").and_then(Value::as_str).unwrap_or_default();
    let snippets:Vec<_>=SNIPPETS.captures_iter(html).map(|capture|strip_html(&capture[1])).collect();
    LINKS.captures_iter(html).enumerate().filter_map(|(index,capture)| {
        let decoded=html_decode(&capture[1]); let absolute=if decoded.starts_with("//") { format!("https:{decoded}") } else { decoded };
        let parsed=url::Url::parse(&absolute).ok()?; let destination=parsed.query_pairs().find(|(key,_)|key=="uddg").map(|(_,value)|value.into_owned()).unwrap_or(absolute);
        result(Some(&strip_html(&capture[2])),Some(&destination),snippets.get(index).map(String::as_str),None,None)
    }).take(50).collect()
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test] fn redirect_entities_and_snippet() {
        let results=normalize_response(&json!({"html":"<a class=\"result__a\" href=\"//duckduckgo.com/l/?uddg=https%3A%2F%2Fa.test\">A &amp; <b>B</b></a><a class=\"result__snippet\">two   words</a>"})); assert_eq!(results[0].title,"A & B"); assert_eq!(results[0].url,"https://a.test"); assert_eq!(results[0].snippet.as_deref(),Some("two words"));
    }
    #[test] fn invalid_links_are_discarded() { assert!(normalize_response(&json!({"html":"<a class=\"result__a\" href=\"/relative\">Title</a>"})).is_empty()); }
}
