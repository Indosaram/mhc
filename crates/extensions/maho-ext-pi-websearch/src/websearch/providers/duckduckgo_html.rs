use std::{collections::BTreeMap,sync::LazyLock};
use serde_json::{Map,Value};
use regex::Regex;
use super::{shared::*,super::{types::*,provider_endpoints::provider_url}};
static LINKS:LazyLock<Regex>=LazyLock::new(||Regex::new(r#"<a\b[^>]*class="[^"]*result__a[^"]*"[^>]*href="([^"]+)"[^>]*>([\s\S]*?)</a>"#).expect("constant link regex"));
static SNIPPETS:LazyLock<Regex>=LazyLock::new(||Regex::new(r#"<a\b[^>]*class="[^"]*result__snippet[^"]*"[^>]*>([\s\S]*?)</a>"#).expect("constant snippet regex"));
static TAGS:LazyLock<Regex>=LazyLock::new(||Regex::new("<[^>]*>").expect("constant tag regex"));
static SPACE:LazyLock<Regex>=LazyLock::new(||Regex::new(r"[\x09-\x0d\x20\u{00a0}\u{1680}\u{2000}-\u{200a}\u{2028}\u{2029}\u{202f}\u{205f}\u{3000}\u{feff}]+").expect("constant ECMAScript whitespace regex"));
fn html_decode(value:&str)->String{value.replace("&amp;","&").replace("&quot;","\"").replace("&#39;","'").replace("&lt;","<").replace("&gt;",">")}
fn strip_html(value:&str)->String{html_decode(SPACE.replace_all(&TAGS.replace_all(value,"")," ").trim_matches(' '))}
pub struct DuckDuckGoHtmlProvider;
impl DuckDuckGoHtmlProvider{
    pub fn build_request(&self,context:&BuildContext<'_>)->Result<BuiltSearchRequest,url::ParseError>{
        let mut url=url::Url::parse(provider_url(context.config.provider,context.config.base_url.as_deref()))?;
        let query=append_domain_filters(&context.request.query,context.allowed_domains.as_deref(),context.blocked_domains.as_deref());
        let mut pairs=url.query_pairs().into_owned().collect::<Vec<_>>();
        if let Some(index)=pairs.iter().position(|(name,_)|name=="q"){pairs[index].1=query;pairs=pairs.into_iter().enumerate().filter(|(position,(name,_))|*position==index||name!="q").map(|(_,pair)|pair).collect();}else{pairs.push(("q".into(),query));}
        url.query_pairs_mut().clear().extend_pairs(pairs);
        Ok(BuiltSearchRequest{url:url.into(),init:SearchRequestInit{method:HttpMethod::Get,headers:BTreeMap::from([("Accept".into(),"text/html".into())])},body:None})
    }
    pub fn normalize_response(&self,data:&Map<String,Value>)->Vec<SearchResultItem>{
        let html=get_string(data.get("html")).unwrap_or("");let snippets=SNIPPETS.captures_iter(html).map(|capture|strip_html(&capture[1])).collect::<Vec<_>>();
        collect(LINKS.captures_iter(html).enumerate().map(|(index,capture)|{let title=strip_html(&capture[2]);let href=html_decode(&capture[1]);let absolute=if href.starts_with("//"){format!("https:{href}")}else{href};let parsed=url::Url::parse(&absolute).ok()?;let redirected=parsed.query_pairs().find(|(key,_)|key=="uddg").map(|(_,value)|value.into_owned());result(Some(&title),Some(redirected.as_deref().unwrap_or(&absolute)),snippets.get(index).map(String::as_str),None,None)}).collect(),50)
    }
}
