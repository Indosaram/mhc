pub struct ReadableArticle{pub title:String,pub content:String,pub has_heading:bool}
// LinkeDOM uses htmlparser2's stack, not the HTML5 tree builder's foster
// parenting, adoption agency, or template fragment construction.
struct WebDocumentSink{document:dom_query::Document,stack:std::cell::RefCell<Vec<(String,dom_query::NodeId)>>}
fn implies_close(open:&str,current:&str)->bool{
    match open{
        "tr"=>matches!(current,"tr"|"th"|"td"),"th"=>current=="th","td"=>matches!(current,"thead"|"th"|"td"),
        "body"=>matches!(current,"head"|"link"|"script"),"li"=>current=="li","option"=>current=="option","optgroup"=>matches!(current,"optgroup"|"option"),
        "dd"|"dt"=>matches!(current,"dd"|"dt"),"rt"|"rp"=>matches!(current,"rt"|"rp"),"tbody"|"tfoot"=>matches!(current,"thead"|"tbody"),
        "select"|"input"|"output"|"button"|"datalist"|"textarea"=>matches!(current,"input"|"option"|"optgroup"|"select"|"button"|"datalist"|"textarea"),
        "p"|"h1"|"h2"|"h3"|"h4"|"h5"|"h6"|"address"|"article"|"aside"|"blockquote"|"details"|"div"|"dl"|"fieldset"|"figcaption"|"figure"|"footer"|"form"|"header"|"hr"|"main"|"nav"|"ol"|"pre"|"section"|"table"|"ul"=>current=="p",
        _=>false,
    }
}
impl html5ever::tokenizer::TokenSink for WebDocumentSink{
    type Handle=dom_query::NodeId;
    fn process_token(&self,token:html5ever::tokenizer::Token,_:u64)->html5ever::tokenizer::TokenSinkResult<Self::Handle>{
        use html5ever::{tokenizer::{Token,TagKind,TokenSinkResult,states::RawKind},tree_builder::{TreeSink,NodeOrText,ElementFlags}};
        let mut stack=self.stack.borrow_mut();
        match token{
            Token::TagToken(tag) if tag.kind==TagKind::StartTag=>{
                let name=tag.name.to_string();while stack.last().is_some_and(|(current,_)|implies_close(&name,current)){stack.pop();}
                let node=self.document.create_element(html5ever::QualName::new(None,html5ever::ns!(html),tag.name),tag.attrs,ElementFlags::default());
                self.document.append(&stack.last().map_or_else(||self.document.get_document(),|(_,id)|*id),NodeOrText::AppendNode(node));
                if !["area","base","basefont","br","col","command","embed","frame","hr","img","input","isindex","keygen","link","meta","param","source","track","wbr"].contains(&name.as_str()){
                    stack.push((name.clone(),node));
                    match name.as_str(){"script"=>return TokenSinkResult::RawData(RawKind::ScriptData),"style"|"xmp"=>return TokenSinkResult::RawData(RawKind::Rawtext),"title"|"textarea"=>return TokenSinkResult::RawData(RawKind::Rcdata),_=>{}}
                }
            }
            Token::TagToken(tag)=>{if let Some(index)=stack.iter().rposition(|(name,_)|name.as_str()==tag.name.as_ref()){stack.truncate(index);}}
            Token::CharacterTokens(text)=>self.document.append(&stack.last().map_or_else(||self.document.get_document(),|(_,id)|*id),NodeOrText::AppendText(text)),
            Token::CommentToken(text)=>{let node=self.document.create_comment(text);self.document.append(&stack.last().map_or_else(||self.document.get_document(),|(_,id)|*id),NodeOrText::AppendNode(node));}
            _=>{}
        }TokenSinkResult::Continue
    }
}
fn parse_web_document(html:&str)->dom_query::Document{
    use html5ever::{tokenizer::{Tokenizer,TokenizerOpts,BufferQueue},tree_builder::{TreeSink,NodeOrText,ElementFlags}};
    fn parse(html:&str)->dom_query::Document{
        let tokenizer=Tokenizer::new(WebDocumentSink{document:dom_query::Document::default(),stack:Default::default()},TokenizerOpts::default());
        let input=BufferQueue::default();input.push_back(html.into());let _=tokenizer.feed(&input);tokenizer.end();tokenizer.sink.document
    }
    let mut document=parse(html);
    if document.root().element_children().first().is_none_or(|node|node.node_name().is_none_or(|name|name.as_ref()!="html")){document=parse(&format!("<html><head></head><body>{html}</body></html>"));}
    let root=document.select("html").first();
    for name in ["head","body"]{if root.children().filter(name).is_empty(){let id=document.create_element(html5ever::QualName::new(None,html5ever::ns!(html),name.into()),Vec::new(),ElementFlags::default());document.append(&root.nodes()[0].id,NodeOrText::AppendNode(id));}}
    let body=document.select("body").first();let head=document.select("head").first();
    for node in root.nodes()[0].children(){if node.id!=body.nodes()[0].id&&node.id!=head.nodes()[0].id{body.nodes()[0].append_child(&node);}}
    for node in body.nodes()[0].element_children(){if node.node_name().is_some_and(|name|["base","link","meta","title","style","script","noscript","template"].contains(&name.as_ref())){head.nodes()[0].append_child(&node);}else{break;}}
    document
}
pub fn extract_readable_article(html: &str, url: &str) -> Option<ReadableArticle> {
    if url::Url::parse(url).is_err() { return None; }
    if let Some(article) = extract_explicit_article(html) { return Some(article); }
    let config = dom_smoothie::Config { char_threshold: 80, keep_classes: false, ..Default::default() };
    let document=parse_web_document(html);
    for node in document.select("xmp").nodes(){for text in node.children().into_iter().filter(dom_query::NodeRef::is_text){text.set_text(text.text().replace('&',"&amp;").replace('<',"&lt;").replace('>',"&gt;").replace('\u{00a0}',"&#160;"));}}
    let mut reader = dom_smoothie::Readability::with_document(document, Some(url), Some(config)).ok()?;
    let article = reader.parse().ok()?;
    if article.content.is_empty() || article.text_content.is_empty() { return None; }
    let mut title = String::new();
    for selector in [".tit_post", ".entry-title", ".post-title", ".article-title", "h1"] {
        title = normalize_plain_text(&reader.doc.select(selector).first().text());
        if !title.is_empty() { break; }
    }
    if title.is_empty() { title = normalize_plain_text(&article.title); }
    let content = article.content.to_string();
    let document = dom_query::Document::fragment(content.as_str());
    let has_heading = !document.select("h1,h2,h3,h4,h5,h6").is_empty();
    Some(ReadableArticle { title, content, has_heading })
}
pub fn html_to_markdown(html: &str, url: &str) -> String {
    let Some(article) = extract_readable_article(html, url) else {
        return normalize_markdown(&html_fragment_to_markdown(html));
    };
    let markdown = normalize_markdown(&html_fragment_to_markdown(&article.content));
    if article.title.is_empty() || article.has_heading || markdown.starts_with(&format!("# {}", article.title)) {
        markdown
    } else { format!("# {}\n\n{markdown}", article.title).trim().to_owned() }
}
pub fn html_to_text(html: &str, url: &str) -> String {
    let Some(article) = extract_readable_article(html, url) else {
        return html_fragment_to_plain_text(html);
    };
    let body = html_fragment_to_plain_text(&article.content);
    if article.title.is_empty() || article.has_heading || body.starts_with(&article.title) {
        body
    } else { format!("{}\n\n{body}", article.title).trim().to_owned() }
}
pub fn html_fragment_to_markdown(html:&str)->String{
    let document=parse_web_document(html);let root=document.select("body").nodes()[0];
    collapse_markdown_whitespace(root);
    markdown_children(root,false).trim_start_matches(['\t','\r','\n']).trim_end_matches(js_whitespace).to_owned()
}
fn markdown_children(node:dom_query::NodeRef<'_>,code:bool)->String{
    node.children().into_iter().fold(String::new(),|output,child|join_markdown(&output,&markdown_node(child,code)))
}
fn markdown_node(node:dom_query::NodeRef<'_>,code:bool)->String{
    let name=node.node_name().map_or_else(String::new,|name|name.to_string());let code=code||name=="code";
    if node.is_text(){return if code{node.text().to_string()}else{escape_markdown(&node.text())};}
    if !node.is_element(){return String::new();}
    let block=MARKDOWN_BLOCKS.contains(&name.as_str());
    let code_child=node.first_child().filter(|child|child.node_name().is_some_and(|name|name.as_ref()=="code"));
    let href=node.attr("href").filter(|href|!href.is_empty());
    let meaningful=["a","table","thead","tbody","tfoot","th","td","iframe","script","audio","video"];
    let has_meaningful=dom_query::Selection::from(node).select("area,base,br,col,command,embed,hr,img,input,keygen,link,meta,param,source,track,wbr,a,table,thead,tbody,tfoot,th,td,iframe,script,audio,video").length()>0;
    let blank=!MARKDOWN_VOIDS.contains(&name.as_str())&&!meaningful.contains(&name.as_str())&&node.text().chars().all(js_whitespace)&&!has_meaningful;
    let text=node.text();let mut leading=if block{String::new()}else{text.chars().take_while(|ch|js_whitespace(*ch)).collect::<String>()};
    let mut trailing=if block||leading.len()==text.len(){String::new()}else{text.chars().rev().take_while(|ch|js_whitespace(*ch)).collect::<String>().chars().rev().collect()};
    let flanked=|sibling:Option<dom_query::NodeRef<'_>>,left:bool|sibling.is_some_and(|sibling|{let block=sibling.node_name().is_some_and(|name|MARKDOWN_BLOCKS.contains(&name.as_ref()));!block&&(sibling.is_text()||sibling.is_element())&&if left{sibling.text().ends_with(' ')}else{sibling.text().starts_with(' ')}});
    if flanked(node.prev_sibling(),true){leading=leading.trim_start_matches([' ','\t','\r','\n']).into();}
    if flanked(node.next_sibling(),false){trailing=trailing.trim_end_matches([' ','\t','\r','\n']).into();}
    let mut content=markdown_children(node,code);if !leading.is_empty()||!trailing.is_empty(){content=content.trim_matches(js_whitespace).into();}
    let replacement=if blank{if block{"\n\n".into()}else{String::new()}}else{match name.as_str(){
        "p"=>format!("\n\n{content}\n\n"),"br"=>"  \n".into(),
        "h1"|"h2"|"h3"|"h4"|"h5"|"h6"=>format!("\n\n{} {content}\n\n","#".repeat(usize::from(name.as_bytes()[1]-b'0'))),
        "blockquote"=>format!("\n\n> {}\n\n",content.trim_matches('\n').replace('\n',"\n> ")),
        "ul"|"ol"=>{let nested=node.parent().is_some_and(|parent|parent.node_name().is_some_and(|name|name.as_ref()=="li")&&parent.element_children().last().is_some_and(|last|last.id==node.id));if nested{format!("\n{content}")}else{format!("\n\n{content}\n\n")}},
        "li"=>{let parent=node.parent();let index=parent.map_or(0,|parent|parent.element_children().iter().position(|child|child.id==node.id).unwrap_or(0));let prefix=if parent.is_some_and(|parent|parent.node_name().is_some_and(|name|name.as_ref()=="ol")){let start=parent.and_then(|parent|parent.attr("start")).filter(|start|!start.is_empty()).map_or(1.0,|start|maho_ai::utils::js::string_to_number(&start));format!("{}.  ",maho_ai::utils::js::number_to_string(start+index as f64))}else{"-   ".into()};let paragraph=content.ends_with('\n');let body=format!("{}{}",content.trim_matches('\n'),if paragraph{"\n"}else{""}).replace('\n',&format!("\n{}"," ".repeat(prefix.len())));format!("{prefix}{body}{}",if node.next_sibling().is_some(){"\n"}else{""})},
        "pre" if code_child.is_some()=>code_child.map_or_else(String::new,|child|{let class=child.attr("class").map_or_else(String::new,|class|class.to_string());let language=class.match_indices("language-").find_map(|(index,_)|class[index+9..].split(js_whitespace).next().filter(|language|!language.is_empty())).unwrap_or("");let text=child.text();let size=text.split(['\n','\r','\u{2028}','\u{2029}']).map(|line|line.bytes().take_while(|byte|*byte==b'`').count()).filter(|count|*count>=3).max().map_or(3,|count|count+1);let fence="`".repeat(size);format!("\n\n{fence}{language}\n{}\n{fence}\n\n",text.strip_suffix('\n').unwrap_or(&text))}),
        "hr"=>"\n\n---\n\n".into(),
        "a" if href.is_some()=>href.map_or_else(String::new,|href|inline_link_markdown(&content,&href,&node.attr("title").unwrap_or_default())),
        "em"|"i"=>if content.trim_matches(js_whitespace).is_empty(){String::new()}else{format!("*{content}*")},
        "strong"|"b"=>if content.trim_matches(js_whitespace).is_empty(){String::new()}else{format!("**{content}**")},
        "code" if !(node.parent().is_some_and(|parent|parent.node_name().is_some_and(|name|name.as_ref()=="pre"))&&node.prev_sibling().is_none()&&node.next_sibling().is_none())=>inline_code_markdown(&content),
        "img"=>image_markdown(&node.attr("alt").unwrap_or_default(),&node.attr("src").unwrap_or_default(),&node.attr("title").unwrap_or_default()),
        "script"|"style"|"noscript"|"iframe"|"object"|"embed"|"meta"|"link"=>String::new(),
        _=>if block{format!("\n\n{content}\n\n")}else{content},
    }};
    format!("{leading}{replacement}{trailing}")
}
pub fn inline_link_markdown(content:&str,href:&str,title:&str)->String{
    let title=clean_markdown_attribute(title).replace('"',"\\\"");let title=if title.is_empty(){String::new()}else{format!(" \"{title}\"")};
    format!("[{content}]({}{title})",escape_link_destination(href))
}
pub fn image_markdown(alt:&str,src:&str,title:&str)->String{
    if src.is_empty(){return String::new();}
    format!("!{}",inline_link_markdown(&escape_markdown(&clean_markdown_attribute(alt)),src,title))
}
pub fn escape_link_destination(destination:&str)->String{
    let mut escaped=String::new();for ch in destination.chars(){if matches!(ch,'<'|'>'|'('|')'){escaped.push('\\');}escaped.push(ch);}
    if escaped.contains(' '){format!("<{escaped}>")}else{escaped}
}
pub fn clean_markdown_attribute(attribute:&str)->String{
    let mut output=String::new();let mut after_newline=false;
    for ch in attribute.chars(){if ch=='\n'{if !after_newline{output.push('\n');}after_newline=true;}else if !(after_newline&&js_whitespace(ch)){output.push(ch);after_newline=false;}}
    output
}
fn js_whitespace(ch:char)->bool{matches!(ch,'\u{0009}'..='\u{000d}'|' '|'\u{00a0}'|'\u{1680}'|'\u{2000}'..='\u{200a}'|'\u{2028}'|'\u{2029}'|'\u{202f}'|'\u{205f}'|'\u{3000}'|'\u{feff}')}
const MARKDOWN_BLOCKS:&[&str]=&["address","article","aside","audio","blockquote","body","canvas","center","dd","dir","div","dl","dt","fieldset","figcaption","figure","footer","form","frameset","h1","h2","h3","h4","h5","h6","header","hgroup","hr","html","isindex","li","main","menu","nav","noframes","noscript","ol","output","p","pre","section","table","tbody","td","tfoot","th","thead","tr","ul"];
const MARKDOWN_VOIDS:&[&str]=&["area","base","br","col","command","embed","hr","img","input","keygen","link","meta","param","source","track","wbr"];
pub fn collapse_markdown_whitespace(root:dom_query::NodeRef<'_>){
    if root.first_child().is_none()||root.node_name().is_some_and(|name|name.as_ref()=="pre"){return;}
    let mut previous_text:Option<dom_query::NodeRef<'_>>=None;let mut keep_leading=false;let mut previous:Option<dom_query::NodeRef<'_>>=None;let mut current=root.first_child();
    while let Some(node)=current{
        if node.id==root.id{break;}
        if node.is_text(){
            let mut text=String::new();let mut whitespace=false;
            for ch in node.text().chars(){if matches!(ch,' '|'\r'|'\n'|'\t'){if !whitespace{text.push(' ');}whitespace=true;}else{text.push(ch);whitespace=false;}}
            if (previous_text.is_none()||previous_text.is_some_and(|prev|prev.text().ends_with(' ')))&&!keep_leading&&text.starts_with(' '){text.remove(0);}
            if text.is_empty(){current=node.next_sibling().or_else(||node.parent());node.remove_from_parent();continue;}
            node.set_text(text);previous_text=Some(node);
        }else if node.is_element(){
            let name=node.node_name().map_or_else(String::new,|name|name.to_string());
            if MARKDOWN_BLOCKS.contains(&name.as_str())||name=="br"{
                if let Some(prev)=previous_text{let text=prev.text();prev.set_text(text.strip_suffix(' ').unwrap_or(&text));}
                previous_text=None;keep_leading=false;
            }else if MARKDOWN_VOIDS.contains(&name.as_str())||name=="pre"{previous_text=None;keep_leading=true;}else if previous_text.is_some(){keep_leading=false;}
        }else{current=node.next_sibling().or_else(||node.parent());node.remove_from_parent();continue;}
        let is_pre=node.node_name().is_some_and(|name|name.as_ref()=="pre");
        current=if previous.and_then(|prev|prev.parent()).is_some_and(|parent|parent.id==node.id)||is_pre{node.next_sibling().or_else(||node.parent())}else{node.first_child().or_else(||node.next_sibling()).or_else(||node.parent())};
        previous=Some(node);
    }
    if let Some(prev)=previous_text{let text=prev.text();let text=text.strip_suffix(' ').unwrap_or(&text);prev.set_text(text);if text.is_empty(){prev.remove_from_parent();}}
}
pub fn inline_code_markdown(content:&str)->String{
    if content.is_empty(){return String::new();}
    let content=content.replace("\r\n"," ").replace(['\r','\n']," ");
    let space=content.starts_with('`')||content.ends_with('`')||(content.starts_with(' ')&&content.ends_with(' ')&&content.chars().any(|ch|ch!=' '));
    let runs=content.split(|ch|ch!='`').filter(|run|!run.is_empty()).map(str::len).collect::<Vec<_>>();
    let mut length=1;while runs.contains(&length){length+=1;}
    let delimiter="`".repeat(length);let padding=if space{" "}else{""};
    format!("{delimiter}{padding}{content}{padding}{delimiter}")
}
pub fn join_markdown(output:&str,replacement:&str)->String{
    let first=output.trim_end_matches('\n');let second=replacement.trim_start_matches('\n');
    let count=(output.len()-first.len()).max(replacement.len()-second.len()).min(2);
    format!("{first}{}{second}","\n".repeat(count))
}
pub fn escape_markdown(text:&str)->String{
    let mut escaped=text.replace('\\',"\\\\").replace('*',"\\*");
    if escaped.starts_with('-')||escaped.starts_with("+ ")||escaped.starts_with('='){escaped.insert(0,'\\');}
    let hashes=escaped.chars().take_while(|ch|*ch=='#').count();
    if (1..=6).contains(&hashes)&&escaped.as_bytes().get(hashes)==Some(&b' '){escaped.insert(0,'\\');}
    escaped=escaped.replace('`',"\\`");
    if escaped.starts_with("~~~"){escaped.insert(0,'\\');}
    escaped=escaped.replace('[',"\\[").replace(']',"\\]");
    if escaped.starts_with('>'){escaped.insert(0,'\\');}
    escaped=escaped.replace('_',"\\_");
    let digits=escaped.bytes().take_while(u8::is_ascii_digit).count();
    if digits>0&&escaped.get(digits..).is_some_and(|tail|tail.starts_with(". ")){escaped.insert(digits,'\\');}
    escaped
}
pub fn extract_explicit_article(html:&str)->Option<ReadableArticle>{
    let document=parse_web_document(html);
    for selector in [".article_view",".tt_article_useless_p_margin",".entry-content",".contents_style",".post-content",".article-content",".content-article","#content .contents_style"]{
        let candidate=document.select(selector).first();if candidate.is_empty(){continue;}
        let cloned=parse_web_document(candidate.html().as_ref());
        let root=cloned.select("body > *").first();
        root.select("script, style, noscript, iframe, object, embed, meta, link, nav, aside, footer, .another_category, .area_related, .related, .revenue_unit_wrap, .adsbygoogle, .container_postbtn, .postbtn_like, .comments, .comment, .tagTrail, .sidebar").remove();
        if normalize_plain_text(&root.text()).encode_utf16().count()<30{continue;}
        let mut title=String::new();for selector in [".tit_post",".entry-title",".post-title",".article-title","h1"]{title=normalize_plain_text(&document.select(selector).first().text());if !title.is_empty(){break;}}
        if title.is_empty(){title=normalize_plain_text(&document.select("title").first().text());}
        let content=root.inner_html().to_string();let has_heading=!root.select("h1,h2,h3,h4,h5,h6").is_empty();
        return Some(ReadableArticle{title,content,has_heading});
    }None
}
pub fn html_fragment_to_plain_text(html:&str)->String{
    let document=parse_web_document(html);
    document.select("script, style, noscript, iframe, object, embed, meta, link").remove();
    document.select("br").replace_with_html("\n");
    document.select("td, th").after_html("\n");
    let blocks=document.select("address, article, aside, blockquote, dd, div, dl, dt, figcaption, figure, footer, h1, h2, h3, h4, h5, h6, header, hr, li, main, nav, ol, p, pre, section, table, tbody, tfoot, thead, tr, ul");
    blocks.before_html("\n");blocks.after_html("\n");
    normalize_plain_text(&document.select("body").text())
}
pub fn decode_html_entities(text:&str)->String{
    let mut output=String::new();let mut rest=text;
    while let Some(start)=rest.find('&'){
        output.push_str(&rest[..start]);rest=&rest[start..];
        let Some(end)=rest.find(';')else{output.push_str(rest);return output;};
        let entity=&rest[1..end];
        let decoded=if let Some(hex)=entity.strip_prefix("#x"){
            if !hex.is_empty()&&hex.chars().all(|ch|ch.is_ascii_hexdigit()){Some(decode_code_point(u32::from_str_radix(hex,16).ok()))}else{None}
        }else if let Some(decimal)=entity.strip_prefix('#'){
            if !decimal.is_empty()&&decimal.chars().all(|ch|ch.is_ascii_digit()){Some(decode_code_point(decimal.parse().ok()))}else{None}
        }else if !entity.is_empty()&&entity.chars().all(|ch|ch.is_ascii_alphabetic()){
            match entity.to_ascii_lowercase().as_str(){"amp"=>Some("&".into()),"apos"=>Some("'".into()),"gt"=>Some(">".into()),"lt"=>Some("<".into()),"nbsp"=>Some(" ".into()),"quot"=>Some("\"".into()),_=>Some(format!("&{entity};"))}
        }else{None};
        if let Some(decoded)=decoded{output.push_str(&decoded);rest=&rest[end+1..];}else{output.push('&');rest=&rest[1..];}
    }
    output.push_str(rest);output
}
fn decode_code_point(point:Option<u32>)->String{point.and_then(char::from_u32).map_or_else(String::new,|point|point.to_string())}
pub fn normalize_plain_text(text:&str)->String{
    let mut output=String::new();let mut whitespace=false;
    for ch in text.chars(){if matches!(ch,'\t'|'\u{000c}'|'\u{000b}'|' '|'\u{00a0}'){if !whitespace{output.push(' ');}whitespace=true;}else{output.push(ch);whitespace=false;}}
    normalize_breaks(&output)
}
pub fn normalize_markdown(markdown:&str)->String{normalize_breaks(&markdown.replace("\r\n","\n").replace('\r',"\n"))}
fn normalize_breaks(text:&str)->String{
    let mut output=String::new();let mut newlines=0;
    for (index,line) in text.split('\n').enumerate(){let line=if index==0{line.trim_end_matches([' ','\t'])}else{line.trim_matches([' ','\t'])};if index!=0{newlines+=1;if newlines<=2{output.push('\n');}}
        if !line.is_empty(){output.push_str(line);newlines=0;}}
    output.trim_matches(|ch|matches!(ch,'\u{0009}'..='\u{000d}'|' '|'\u{00a0}'|'\u{1680}'|'\u{2000}'..='\u{200a}'|'\u{2028}'|'\u{2029}'|'\u{202f}'|'\u{205f}'|'\u{3000}'|'\u{feff}')).into()
}
