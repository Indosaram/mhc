use url::Url;
use std::cell::RefCell;
use html5ever::tokenizer::{Token,TokenSink,TokenSinkResult,Tokenizer,TokenizerOpts,TagKind,BufferQueue,states::RawKind};
use html5ever::{QualName,ns};
struct InertParser {document:dom_query::Document,stack:RefCell<Vec<dom_query::NodeId>>}
fn is_void(name:&str)->bool {matches!(name,"area"|"base"|"basefont"|"br"|"col"|"command"|"embed"|"frame"|"hr"|"img"|"input"|"isindex"|"keygen"|"link"|"meta"|"param"|"source"|"track"|"wbr")}
fn implies_close(open:&str,current:&str)->bool {
    match open {
        "tr"=>matches!(current,"tr"|"th"|"td"),"th"=>current=="th","td"=>matches!(current,"thead"|"th"|"td"),"body"=>matches!(current,"head"|"link"|"script"),"li"=>current=="li",
        "select"|"input"|"output"|"button"|"datalist"|"textarea"=>matches!(current,"input"|"option"|"optgroup"|"select"|"button"|"datalist"|"textarea"),
        "option"=>current=="option","optgroup"=>matches!(current,"optgroup"|"option"),"dd"|"dt"=>matches!(current,"dd"|"dt"),"rt"|"rp"=>matches!(current,"rt"|"rp"),"tbody"|"tfoot"=>matches!(current,"thead"|"tbody"),
        "p"|"h1"|"h2"|"h3"|"h4"|"h5"|"h6"|"address"|"article"|"aside"|"blockquote"|"details"|"div"|"dl"|"fieldset"|"figcaption"|"figure"|"footer"|"form"|"header"|"hr"|"main"|"nav"|"ol"|"pre"|"section"|"table"|"ul"=>current=="p",_=>false,
    }
}
impl TokenSink for InertParser {
    type Handle=();
    fn process_token(&self,token:Token,_line:u64)->TokenSinkResult<()> {
        let tree=&self.document.tree;let mut stack=self.stack.borrow_mut();
        let parent=|stack:&[dom_query::NodeId]|stack.last().map_or_else(||self.document.root(),|id|tree.get_unchecked(id));
        match token {
            Token::TagToken(tag) if tag.kind==TagKind::StartTag=> {
                let name=tag.name.as_ref();while stack.last().is_some_and(|id|tree.get_unchecked(id).node_name().is_some_and(|current|implies_close(name,&current))) {stack.pop();}
                let foreign=stack.iter().any(|id|tree.get_unchecked(id).node_name().as_deref()==Some("svg"));let namespace=if foreign||name=="svg" {ns!(svg)} else {ns!(html)};
                let node=tree.create_node(dom_query::NodeData::Element(dom_query::Element::new(QualName::new(None,namespace,tag.name.clone()),tag.attrs,None,false)));parent(&stack).append_child(&node);
                if !(is_void(name)||foreign&&tag.self_closing) {stack.push(node);}
                return match name {"script"|"style"|"xmp"=>TokenSinkResult::RawData(RawKind::Rawtext),"title"|"textarea"=>TokenSinkResult::RawData(RawKind::Rcdata),_=>TokenSinkResult::Continue};
            },
            Token::TagToken(tag)=> {
                if let Some(index)=stack.iter().rposition(|id|tree.get_unchecked(id).node_name().as_deref()==Some(tag.name.as_ref())) {stack.truncate(index);}
                else if matches!(tag.name.as_ref(),"p"|"br") {let node=tree.create_node(dom_query::NodeData::Element(dom_query::Element::new(QualName::new(None,ns!(html),tag.name),vec![],None,false)));parent(&stack).append_child(&node);}
            },
            Token::CharacterTokens(contents)=> {
                let parent=parent(&stack);let previous=parent.children().last().copied().filter(|node|node.is_text());
                if matches!(parent.node_name().as_deref(),Some("script"|"style"|"xmp"))&&let Some(previous)=previous {previous.set_text(format!("{}{contents}",previous.text()));}
                else {let node=tree.create_node(dom_query::NodeData::Text{contents});parent.append_child(&node);}
            },
            Token::CommentToken(contents)=> {let node=tree.create_node(dom_query::NodeData::Comment{contents});parent(&stack).append_child(&node);},
            Token::NullCharacterToken=> {let node=tree.create_node(dom_query::NodeData::Text{contents:"\0".into()});parent(&stack).append_child(&node);},_=>{},
        }
        TokenSinkResult::Continue
    }
}
fn parse_inert(html:&str)->dom_query::Document {
    let input=BufferQueue::default();input.push_back(html.into());let tokenizer=Tokenizer::new(InertParser{document:dom_query::Document::default(),stack:RefCell::new(vec![])},TokenizerOpts::default());let _=tokenizer.feed(&input);tokenizer.end();tokenizer.sink.document
}
#[derive(Clone)]
pub struct WebDocument { pub document:dom_query::Document,pub url:String,pub document_uri:String,pub base_uri:String }
pub fn parse_web_document(html:&str,url:&str)->WebDocument {
    let parsed=parse_inert(html);let document=if parsed.root().first_element_child().is_some_and(|node|node.node_name().as_deref()==Some("html")) {parsed} else {parse_inert(&format!("<html><head></head><body>{html}</body></html>"))};
    let html=document.root().first_element_child().expect("HTML container");
    let head=if let Some(head)=html.first_element_child().filter(|node|node.node_name().as_deref()==Some("head")) {head} else {let id=document.tree.create_node(dom_query::NodeData::Element(dom_query::Element::new(QualName::new(None,ns!(html),"head".into()),vec![],None,false)));html.prepend_child(&id);document.tree.get_unchecked(&id)};
    let body=if let Some(body)=head.next_element_sibling().filter(|node|node.node_name().as_deref()==Some("body")) {body} else {let id=document.tree.create_node(dom_query::NodeData::Element(dom_query::Element::new(QualName::new(None,ns!(html),"body".into()),vec![],None,false)));let body=document.tree.get_unchecked(&id);head.insert_after(&body);body};
    for node in html.children() {if node.id!=head.id&&node.id!=body.id {body.append_child(&node);}}
    for element in body.element_children() {if !matches!(element.node_name().as_deref(),Some("base"|"link"|"meta"|"title"|"style"|"script"|"noscript"|"template")) {break;}head.append_child(&element);}
    apply_web_document_url(document,url)
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
    #[test] fn inert_stack_parser_does_not_reconstruct_misnested_formatting() {
        let web=parse_web_document("<b>first<i>second</b>third</i>","https://example.test/");assert_eq!(web.document.select("body").inner_html().as_ref(),"<b>first<i>second</i></b>third");
    }
    #[test] fn inert_stack_parser_relocates_only_leading_metadata() {
        let web=parse_web_document("<title>Leading</title><p>body</p><title>Trailing</title>","https://example.test/");assert_eq!(web.document.select("head title").text().as_ref(),"Leading");assert_eq!(web.document.select("body title").text().as_ref(),"Trailing");
    }
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
