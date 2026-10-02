use url::Url;
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
    #[test] fn base_href_resolves_relative_and_invalid_href_uses_document_url() { assert_eq!(document_base_uri(Some("../assets/"),"https://example.com/posts/a"),"https://example.com/assets/"); assert_eq!(document_base_uri(Some("http://["),"https://example.com/a"),"https://example.com/a"); }
    #[test] fn fragments_stay_relative_only_without_base_override() { assert_eq!(normalize_web_url(true,"#section","https://example.com/a","https://example.com/a"),NormalizedWebUrl::Preserve); assert_eq!(normalize_web_url(true,"#section","https://example.com/base/","https://example.com/a"),NormalizedWebUrl::Set("https://example.com/base/#section".into())); }
    #[test] fn javascript_anchors_are_unwrapped_but_image_sources_are_normalized() { assert_eq!(normalize_web_url(true,"javascript:alert(1)","https://example.com/","https://example.com/"),NormalizedWebUrl::UnwrapAnchor); assert_eq!(normalize_web_url(false,"image.png","https://example.com/","https://example.com/"),NormalizedWebUrl::Set("https://example.com/image.png".into())); }
}
