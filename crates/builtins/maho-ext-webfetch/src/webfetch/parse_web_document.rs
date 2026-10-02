use url::Url;
#[derive(Clone)]
pub struct WebDocument { pub document:dom_query::Document,pub url:String,pub document_uri:String,pub base_uri:String }
pub fn parse_web_document(html:&str,url:&str)->WebDocument {
    apply_web_document_url(dom_query::Document::from(html),url)
}
pub fn apply_web_document_url(document:dom_query::Document,url:&str)->WebDocument {
    let href=document.select("base[href]").attr("href");
    let base_uri=document_base_uri(href.as_deref(),url);
    WebDocument{document,url:url.into(),document_uri:url.into(),base_uri}
}
pub fn normalize_web_urls(root:&dom_query::Selection<'_>,document:&WebDocument) {
    for node in root.select("a[href], img[src]").nodes() {
        let is_anchor=node.node_name().as_deref()==Some("a");let attribute=if is_anchor {"href"} else {"src"};
        match normalize_web_url(is_anchor,&node.attr(attribute).unwrap_or_default(),&document.base_uri,&document.url) {
            NormalizedWebUrl::Preserve=>{},NormalizedWebUrl::Set(value)=>node.set_attr(attribute,&value),
            NormalizedWebUrl::UnwrapAnchor=>{for child in node.children() {node.insert_before(&child);}node.remove_from_parent();}
        }
    }
}
pub fn resolve_web_url(value:&str,base:&str)->Option<Url> { Url::parse(value).ok().or_else(||Url::parse(base).ok()?.join(value).ok()) }
pub fn document_base_uri(href:Option<&str>,document_url:&str)->String { href.and_then(|href|resolve_web_url(href,document_url)).map_or_else(||document_url.into(),|url|url.into()) }
#[derive(Clone,Debug,PartialEq,Eq)]
pub enum NormalizedWebUrl { Preserve,Set(String),UnwrapAnchor }
pub fn normalize_web_url(is_anchor:bool,value:&str,base_uri:&str,document_url:&str)->NormalizedWebUrl {
    let destination=resolve_web_url(value,base_uri);
    if is_anchor && destination.as_ref().is_some_and(|url|url.scheme()=="javascript") { return NormalizedWebUrl::UnwrapAnchor; }
    if value.starts_with('#') && base_uri==document_url { return NormalizedWebUrl::Preserve; }
    destination.map_or(NormalizedWebUrl::Preserve,|url|NormalizedWebUrl::Set(url.into()))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn cloned_document_retains_reapplied_identity_and_first_base() {
        let source=parse_web_document("<base href='../assets/'><base href='https://ignored.test/'><p>Article</p>","https://example.test/posts/final");
        let clone=apply_web_document_url(source.document.clone(),&source.url);
        assert_eq!(clone.url,"https://example.test/posts/final");assert_eq!(clone.document_uri,clone.url);assert_eq!(clone.base_uri,"https://example.test/assets/");
    }
    #[test] fn malformed_first_base_does_not_use_second() {
        let document=parse_web_document("<base href='http://['><base href='https://ignored.test/'><p>Article</p>","https://example.test/posts/final");assert_eq!(document.base_uri,document.url);
    }
    #[test] fn script_anchor_unwrap_preserves_nested_inline_nodes() {
        let document=parse_web_document("<a href=' JaVaScRiPt:alert(1)'><strong>Nested</strong> text</a>","https://example.test/posts/final");
        normalize_web_urls(&document.document.select("body"),&document);
        assert!(document.document.select("a").nodes().is_empty());assert_eq!(document.document.select("strong").text().as_ref(),"Nested");assert_eq!(document.document.select("body").text().as_ref(),"Nested text");
    }
    #[test] fn special_and_malformed_destinations_are_preserved() {
        for destination in ["mailto:reader@example.test","tel:+12025550123","data:image/png;base64,AA==","http://["] {
            let document=parse_web_document(&format!("<a href='{destination}'>Link</a><img src='{destination}'>"),"https://example.test/posts/final");normalize_web_urls(&document.document.select("body"),&document);
            assert_eq!(document.document.select("a").attr("href").as_deref(),Some(destination));assert_eq!(document.document.select("img").attr("src").as_deref(),Some(destination));
        }
    }
    #[test] fn consecutive_documents_keep_relative_url_state_isolated() {
        let first=parse_web_document("<a href='child'>link</a>","https://previous.test/first/");let second=parse_web_document("<a href='child'>link</a>","https://example.test/posts/final");
        normalize_web_urls(&first.document.select("body"),&first);normalize_web_urls(&second.document.select("body"),&second);
        assert_eq!(first.document.select("a").attr("href").as_deref(),Some("https://previous.test/first/child"));assert_eq!(second.document.select("a").attr("href").as_deref(),Some("https://example.test/posts/child"));
    }
    #[test] fn base_href_resolves_relative_and_invalid_href_uses_document_url() { assert_eq!(document_base_uri(Some("../assets/"),"https://example.com/posts/a"),"https://example.com/assets/"); assert_eq!(document_base_uri(Some("http://["),"https://example.com/a"),"https://example.com/a"); }
    #[test] fn fragments_stay_relative_only_without_base_override() { assert_eq!(normalize_web_url(true,"#section","https://example.com/a","https://example.com/a"),NormalizedWebUrl::Preserve); assert_eq!(normalize_web_url(true,"#section","https://example.com/base/","https://example.com/a"),NormalizedWebUrl::Set("https://example.com/base/#section".into())); }
    #[test] fn javascript_anchors_are_unwrapped_but_image_sources_are_normalized() { assert_eq!(normalize_web_url(true,"javascript:alert(1)","https://example.com/","https://example.com/"),NormalizedWebUrl::UnwrapAnchor); assert_eq!(normalize_web_url(false,"image.png","https://example.com/","https://example.com/"),NormalizedWebUrl::Set("https://example.com/image.png".into())); }
}
