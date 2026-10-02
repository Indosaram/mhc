use std::sync::LazyLock;
use regex::Regex;
static WHITESPACE:LazyLock<Regex>=LazyLock::new(||Regex::new(r"[\t\x0c\x0b \u{00a0}]+").expect("literal pattern"));
static BEFORE_NEWLINE:LazyLock<Regex>=LazyLock::new(||Regex::new(r"[ \t]+\n").expect("literal pattern"));
static AFTER_NEWLINE:LazyLock<Regex>=LazyLock::new(||Regex::new(r"\n[ \t]+").expect("literal pattern"));
static NEWLINES:LazyLock<Regex>=LazyLock::new(||Regex::new(r"\n{3,}").expect("literal pattern"));
pub fn reader_inner_text(node:&dom_query::NodeRef<'_>,normalize_spaces:bool)->String {
    let text=node.text();let text=text.trim_matches(js_whitespace);if !normalize_spaces {return text.into();}
    let mut output=String::new();let mut whitespace=String::new();
    for character in text.chars() {if js_whitespace(character) {whitespace.push(character);} else {if whitespace.chars().count()>=2 {output.push(' ');} else {output.push_str(&whitespace);}whitespace.clear();output.push(character);}}
    output.push_str(&whitespace);output
}
pub fn reader_link_density(node:&dom_query::NodeRef<'_>)->f64 {
    let length=reader_inner_text(node,true).encode_utf16().count();if length==0 {return 0.;}
    let links=dom_query::Selection::from(*node).select("a");let mut linked=0.;
    for link in links.nodes() {let coefficient=if link.attr("href").is_some_and(|href|href.starts_with('#')&&href.len()>1) {0.3} else {1.};linked+=reader_inner_text(link,true).encode_utf16().count() as f64*coefficient;}
    linked/length as f64
}
pub fn reader_text_density(node:&dom_query::NodeRef<'_>,tags:&[&str])->f64 {
    fn descendants_length(node:&dom_query::NodeRef<'_>,tags:&[&str])->usize {
        node.element_children().iter().map(|child| {
            let length=if child.node_name().is_some_and(|name|tags.contains(&name.as_ref())) {reader_inner_text(child,true).encode_utf16().count()} else {0};
            length+descendants_length(child,tags)
        }).sum()
    }
    let length=reader_inner_text(node,true).encode_utf16().count();if length==0 {return 0.;}
    descendants_length(node,tags) as f64/length as f64
}
pub fn reader_initial_score(node:&dom_query::NodeRef<'_>,weight_classes:bool)->i32 {
    static POSITIVE:LazyLock<Regex>=LazyLock::new(||Regex::new("(?i)article|body|content|entry|hentry|h-entry|main|page|pagination|post|text|blog|story").expect("literal pattern"));
    static NEGATIVE:LazyLock<Regex>=LazyLock::new(||Regex::new("(?i)-ad-|hidden|^hid$| hid$| hid |^hid |banner|combx|comment|com-|contact|footer|gdpr|masthead|media|meta|outbrain|promo|related|scroll|share|shoutbox|sidebar|skyscraper|sponsor|shopping|tags|widget").expect("literal pattern"));
    let name=node.node_name();let mut score=match name.as_deref() {Some("div")=>5,Some("pre"|"td"|"blockquote")=>3,Some("address"|"ol"|"ul"|"dl"|"dd"|"dt"|"li"|"form")=>-3,Some("h1"|"h2"|"h3"|"h4"|"h5"|"h6"|"th")=>-5,_=>0};
    if weight_classes {
        for attribute in ["class","id"] {
            if let Some(value)=node.attr(attribute) {
                if NEGATIVE.is_match(&value) {score-=25;}
                if POSITIVE.is_match(&value) {score+=25;}
            }
        }
    }
    score
}
pub fn reader_has_single_tag(node:&dom_query::NodeRef<'_>,tag:&str)->bool {
    let children=node.element_children();children.len()==1 && children[0].node_name().as_deref()==Some(tag)
        && !node.children().iter().any(|child|child.is_text()&&child.text().chars().last().is_some_and(|character|!js_whitespace(character)))
}
pub fn reader_element_without_content(node:&dom_query::NodeRef<'_>)->bool {
    let children=node.element_children();node.is_element()&&node.text().trim_matches(js_whitespace).is_empty()
        && (children.is_empty()||children.len()==dom_query::Selection::from(*node).select("br, hr").nodes().len())
}
pub fn reader_has_child_block(node:&dom_query::NodeRef<'_>)->bool {
    node.children().iter().any(|child|child.node_name().is_some_and(|name|matches!(name.as_ref(),"blockquote"|"dl"|"div"|"img"|"ol"|"p"|"pre"|"table"|"ul"))||reader_has_child_block(child))
}
pub fn reader_is_phrasing(node:&dom_query::NodeRef<'_>)->bool {
    if node.is_text() {return true;}
    let name=node.node_name();let name=name.as_deref().unwrap_or("");
    matches!(name,"abbr"|"audio"|"b"|"bdo"|"br"|"button"|"cite"|"code"|"data"|"datalist"|"dfn"|"em"|"embed"|"i"|"img"|"input"|"kbd"|"label"|"mark"|"math"|"meter"|"noscript"|"object"|"output"|"progress"|"q"|"ruby"|"samp"|"script"|"select"|"small"|"span"|"strong"|"sub"|"sup"|"textarea"|"time"|"var"|"wbr")
        || (matches!(name,"a"|"del"|"ins")&&node.children().iter().all(reader_is_phrasing))
}
pub fn reader_is_whitespace(node:&dom_query::NodeRef<'_>)->bool {
    (node.is_text()&&node.text().trim_matches(js_whitespace).is_empty())||node.node_name().as_deref()==Some("br")
}
pub fn reader_prepare_div<'a>(node:dom_query::NodeRef<'a>)->dom_query::NodeRef<'a> {
    let mut paragraph:Option<dom_query::NodeRef<'a>>=None;
    for child in node.children() {
        if reader_is_phrasing(&child) {
            if let Some(paragraph)=paragraph {paragraph.append_child(&child);}
            else if !reader_is_whitespace(&child) {
                child.before_html("<p></p>");let created=child.prev_sibling().expect("inserted paragraph");created.append_child(&child);paragraph=Some(created);
            }
        } else if let Some(current)=paragraph {
            while let Some(last)=current.children().last().copied().filter(reader_is_whitespace) {last.remove_from_parent();}
            paragraph=None;
        }
    }
    if reader_has_single_tag(&node,"p")&&reader_link_density(&node)<0.25 {
        let paragraph=node.element_children()[0];node.replace_with(&paragraph);paragraph
    } else if !reader_has_child_block(&node) {node.rename("p");node} else {node}
}
pub fn reader_replace_breaks(root:&dom_query::NodeRef<'_>) {
    fn next_nonspace(mut node:Option<dom_query::NodeRef<'_>>)->Option<dom_query::NodeRef<'_>> {
        while let Some(current)=node {if current.is_element()||!current.text().chars().all(js_whitespace) {break;}node=current.next_sibling();}node
    }
    let selection=dom_query::Selection::from(*root).select("br");
    for br in selection.nodes() {
        if br.parent().is_none() {continue;}
        let mut next=br.next_sibling();let mut replaced=false;
        while let Some(current)=next_nonspace(next).filter(|node|node.node_name().as_deref()==Some("br")) {
            replaced=true;next=current.next_sibling();current.remove_from_parent();
        }
        if !replaced {continue;}
        br.rename("p");br.remove_all_attrs();next=br.next_sibling();
        while let Some(current)=next {
            if current.node_name().as_deref()==Some("br")&&next_nonspace(current.next_sibling()).is_some_and(|node|node.node_name().as_deref()==Some("br")) {break;}
            if !reader_is_phrasing(&current) {break;}
            next=current.next_sibling();br.append_child(&current);
        }
        while let Some(last)=br.children().last().copied().filter(reader_is_whitespace) {last.remove_from_parent();}
        if let Some(parent)=br.parent().filter(|parent|parent.node_name().as_deref()==Some("p")) {parent.rename("div");}
    }
}
pub fn reader_prepare_document(document:&dom_query::Document) {
    document.select("style").remove();
    if let Some(body)=document.select("body").nodes().first() {reader_replace_breaks(body);}
    for node in document.select("font").nodes() {node.rename("span");}
}
pub fn reader_next_node(mut node:dom_query::NodeRef<'_>,ignore_children:bool)->Option<dom_query::NodeRef<'_>> {
    if !ignore_children && let Some(child)=node.first_element_child() {return Some(child);}
    loop {
        if let Some(sibling)=node.next_element_sibling() {return Some(sibling);}
        node=node.parent()?;
    }
}
pub fn reader_text_similarity(title:&str,heading:&str)->f64 {
    let tokens=|text:&str|text.to_lowercase().split(|character:char|!(character.is_ascii_alphanumeric()||character=='_')).filter(|token|!token.is_empty()).map(str::to_owned).collect::<Vec<_>>();
    let title=tokens(title);let heading=tokens(heading);if title.is_empty()||heading.is_empty() {return 0.;}
    let unique:Vec<_>=heading.iter().filter(|token|!title.contains(token)).cloned().collect();
    1.-unique.join(" ").len() as f64/heading.join(" ").len() as f64
}
pub fn reader_header_duplicates_title(node:&dom_query::NodeRef<'_>,title:&str)->bool {
    matches!(node.node_name().as_deref(),Some("h1"|"h2"))&&reader_text_similarity(title,&reader_inner_text(node,false))>0.75
}
pub fn reader_valid_byline(node:&dom_query::NodeRef<'_>,match_string:&str)->bool {
    static BYLINE:LazyLock<Regex>=LazyLock::new(||Regex::new("(?i)byline|author|dateline|writtenby|p-author").expect("literal pattern"));
    let length=node.text().trim_matches(js_whitespace).encode_utf16().count();
    (node.attr("rel").as_deref()==Some("author")||node.attr("itemprop").is_some_and(|value|value.contains("author"))||BYLINE.is_match(match_string))&&length>0&&length<100
}
pub fn reader_unlikely_candidate(node:&dom_query::NodeRef<'_>,match_string:&str)->bool {
    static UNLIKELY:LazyLock<Regex>=LazyLock::new(||Regex::new("(?i)-ad-|ai2html|banner|breadcrumbs|combx|comment|community|cover-wrap|disqus|extra|footer|gdpr|header|legends|menu|related|remark|replies|rss|shoutbox|sidebar|skyscraper|social|sponsor|supplemental|ad-break|agegate|pagination|pager|popup|yom-remote").expect("literal pattern"));
    static MAYBE:LazyLock<Regex>=LazyLock::new(||Regex::new("(?i)and|article|body|column|content|main|shadow").expect("literal pattern"));
    let mut ancestor=node.parent();let mut protected=false;
    for _ in 0..4 {let Some(parent)=ancestor else {break;};if matches!(parent.node_name().as_deref(),Some("table"|"code")) {protected=true;break;}ancestor=parent.parent();}
    let class_unlikely=UNLIKELY.is_match(match_string)&&!MAYBE.is_match(match_string)&&!protected&&!matches!(node.node_name().as_deref(),Some("body"|"a"));
    class_unlikely||node.attr("role").is_some_and(|role|matches!(role.as_ref(),"menu"|"menubar"|"complementary"|"navigation"|"alert"|"alertdialog"|"dialog"))
}
pub fn reader_probably_visible(node:&dom_query::NodeRef<'_>)->bool {
    let style=node.attr("style").unwrap_or_default();let mut display="";let mut visibility="";
    for rule in style.split(';') {
        if let Some((key,value))=rule.split_once(':') {let key=key.trim_matches(js_whitespace);let value=value.trim_matches(js_whitespace);if value.is_empty() {continue;}
            match key {"display"=>display=value,"visibility"=>visibility=value,_=>{}}
        }
    }
    display!="none"&&visibility!="hidden"&&!node.has_attr("hidden")
        && (node.attr("aria-hidden").as_deref()!=Some("true")||node.attr("class").is_some_and(|class|class.contains("fallback-image")))
}
pub fn score_reader_candidates(elements:&[dom_query::NodeRef<'_>],weight_classes:bool)->Vec<(dom_query::NodeId,f64)> {
    let mut candidates:Vec<(dom_query::NodeRef<'_>,f64)>=Vec::new();
    for element in elements {
        if element.parent().is_none_or(|parent|!parent.is_element()) {continue;}
        let text=reader_inner_text(element,true);let length=text.encode_utf16().count();if length<25 {continue;}
        let commas=text.chars().filter(|character|matches!(character,','|'\u{060c}'|'\u{fe50}'|'\u{fe10}'|'\u{fe11}'|'\u{2e41}'|'\u{2e34}'|'\u{2e32}'|'\u{ff0c}')).count();
        let content_score=2.+commas as f64+(length/100).min(3) as f64;
        let mut ancestor=element.parent();
        for level in 0..5 {
            let Some(node)=ancestor else {break;};ancestor=node.parent();
            if !node.is_element() || node.parent().is_none_or(|parent|!parent.is_element()) {continue;}
            let index=if let Some(index)=candidates.iter().position(|(candidate,_)|candidate.id==node.id) {index} else {candidates.push((node,f64::from(reader_initial_score(&node,weight_classes))));candidates.len()-1};
            let divider=match level {0=>1,1=>2,_=>level*3};candidates[index].1+=content_score/divider as f64;
        }
    }
    for (node,score) in &mut candidates {*score*=1.-reader_link_density(node);}
    candidates.into_iter().map(|(node,score)|(node.id,score)).collect()
}
pub fn reader_top_candidates(candidates:&[(dom_query::NodeId,f64)])->Vec<(dom_query::NodeId,f64)> {
    let mut top:Vec<(dom_query::NodeId,f64)>=Vec::new();
    for candidate in candidates {
        if let Some(index)=(0..5).find(|index|top.get(*index).is_none_or(|current|candidate.1>current.1)) {
            top.insert(index,*candidate);top.truncate(5);
        }
    }
    top
}
pub fn reader_include_sibling(sibling:&dom_query::NodeRef<'_>,top:&dom_query::NodeRef<'_>,top_score:f64,sibling_score:Option<f64>)->bool {
    if sibling.id==top.id {return true;}
    let class=top.attr("class").unwrap_or_default();let bonus=if !class.is_empty()&&sibling.attr("class").unwrap_or_default()==class {top_score*0.2} else {0.};
    if sibling_score.is_some_and(|score|score+bonus>=10_f64.max(top_score*0.2)) {return true;}
    if sibling.node_name().as_deref()!=Some("p") {return false;}
    let text=reader_inner_text(sibling,true);let length=text.encode_utf16().count();let density=reader_link_density(sibling);
    (length>80&&density<0.25)||(length<80&&length>0&&density==0.&&(text.ends_with('.')||text.contains(". ")))
}
fn markdown_block(name:&str)->bool {matches!(name,"address"|"article"|"aside"|"audio"|"blockquote"|"body"|"canvas"|"center"|"dd"|"dir"|"div"|"dl"|"dt"|"fieldset"|"figcaption"|"figure"|"footer"|"form"|"frameset"|"h1"|"h2"|"h3"|"h4"|"h5"|"h6"|"header"|"hgroup"|"hr"|"html"|"isindex"|"li"|"main"|"menu"|"nav"|"noframes"|"noscript"|"ol"|"output"|"p"|"pre"|"section"|"table"|"tbody"|"td"|"tfoot"|"th"|"thead"|"tr"|"ul")}
fn markdown_void(name:&str)->bool {matches!(name,"area"|"base"|"br"|"col"|"command"|"embed"|"hr"|"img"|"input"|"keygen"|"link"|"meta"|"param"|"source"|"track"|"wbr")}
fn markdown_meaningful(name:&str)->bool {matches!(name,"a"|"table"|"thead"|"tbody"|"tfoot"|"th"|"td"|"iframe"|"script"|"audio"|"video")}
pub fn markdown_blank(node:&dom_query::NodeRef<'_>)->bool {
    fn contains_meaningful(node:&dom_query::NodeRef<'_>)->bool {node.children().iter().any(|child| {let name=child.node_name();let name=name.as_deref().unwrap_or("");markdown_void(name)||markdown_meaningful(name)||contains_meaningful(child)})}
    let name=node.node_name();let name=name.as_deref().unwrap_or("");!markdown_void(name)&&!markdown_meaningful(name)&&node.text().chars().all(js_whitespace)&&!contains_meaningful(node)
}
pub fn markdown_flanking(node:&dom_query::NodeRef<'_>)->(String,String) {
    if node.node_name().is_some_and(|name|markdown_block(&name)) {return (String::new(),String::new());}
    let text=node.text();let leading_len=text.len()-text.trim_start_matches(js_whitespace).len();
    let trailing_start=if leading_len==text.len() {text.len()} else {text.trim_end_matches(js_whitespace).len()};
    let mut leading=text[..leading_len].to_owned();let mut trailing=text[trailing_start..].to_owned();
    let flanked=|sibling:Option<dom_query::NodeRef<'_>>,left:bool|sibling.is_some_and(|sibling| {
        let name=sibling.node_name();if !sibling.is_text() && (!sibling.is_element() || markdown_block(name.as_deref().unwrap_or(""))) {return false;}
        if left {sibling.text().ends_with(' ')} else {sibling.text().starts_with(' ')}
    });
    let ascii=|character:char|matches!(character,' '|'\t'|'\r'|'\n');
    if flanked(node.prev_sibling(),true) {leading=leading.trim_start_matches(ascii).into();}
    if flanked(node.next_sibling(),false) {trailing=trailing.trim_end_matches(ascii).into();}
    (leading,trailing)
}
pub fn collapse_markdown_whitespace(root:&dom_query::NodeRef<'_>) {
    fn next<'a>(previous:Option<dom_query::NodeRef<'a>>,current:dom_query::NodeRef<'a>)->Option<dom_query::NodeRef<'a>> {
        if previous.and_then(|node|node.parent()).is_some_and(|parent|parent.id==current.id) || current.node_name().as_deref()==Some("pre") {current.next_sibling().or_else(||current.parent())}
        else {current.children().first().copied().or_else(||current.next_sibling()).or_else(||current.parent())}
    }
    static ASCII_SPACE:LazyLock<Regex>=LazyLock::new(||Regex::new(r"[ \r\n\t]+").expect("literal pattern"));
    if root.children().is_empty() || root.node_name().as_deref()==Some("pre") {return;}
    let mut previous=None;let mut previous_text:Option<dom_query::NodeRef<'_>>=None;let mut keep_leading=false;let mut current=next(previous,*root);
    while let Some(node)=current {
        if node.id==root.id {break;}
        if node.is_text() {
            let mut text=ASCII_SPACE.replace_all(&node.text()," ").into_owned();
            if previous_text.is_none_or(|node|node.text().ends_with(' ')) && !keep_leading && text.starts_with(' ') {text.remove(0);}
            if text.is_empty() {current=node.next_sibling().or_else(||node.parent());node.remove_from_parent();continue;}
            node.set_text(text);previous_text=Some(node);
        } else if node.is_element() {
            let name=node.node_name();let name=name.as_deref().unwrap_or("");
            if markdown_block(name) || name=="br" {if let Some(text)=previous_text {text.set_text(text.text().strip_suffix(' ').unwrap_or(&text.text()));}previous_text=None;keep_leading=false;}
            else if markdown_void(name) || name=="pre" {previous_text=None;keep_leading=true;}
            else if previous_text.is_some() {keep_leading=false;}
        } else {current=node.next_sibling().or_else(||node.parent());node.remove_from_parent();continue;}
        current=next(previous,node);previous=Some(node);
    }
    if let Some(node)=previous_text {let text=node.text();node.set_text(text.strip_suffix(' ').unwrap_or(&text));if node.text().is_empty() {node.remove_from_parent();}}
}
pub fn escape_markdown(text:&str)->String {
    let mut value=text.replace('\\',"\\\\").replace('*',"\\*");
    if value.starts_with('-') {value.insert(0,'\\');}
    if value.starts_with("+ ") {value.insert(0,'\\');}
    if value.starts_with('=') {value.insert(0,'\\');}
    let hashes=value.bytes().take_while(|byte|*byte==b'#').count();if (1..=6).contains(&hashes) && value.as_bytes().get(hashes)==Some(&b' ') {value.insert(0,'\\');}
    value=value.replace('`',"\\`");if value.starts_with("~~~") {value.insert(0,'\\');}
    value=value.replace('[',"\\[").replace(']',"\\]");if value.starts_with('>') {value.insert(0,'\\');}
    value=value.replace('_',"\\_");let digits=value.bytes().take_while(u8::is_ascii_digit).count();
    if digits>0 && value.get(digits..).is_some_and(|rest|rest.starts_with(". ")) {value.insert(digits,'\\');}
    value
}
pub fn escape_link_destination(destination:&str)->String {
    let mut escaped=String::new();for character in destination.chars() {if matches!(character,'<'|'>'|'('|')') {escaped.push('\\');}escaped.push(character);}
    if escaped.contains(' ') {format!("<{escaped}>")} else {escaped}
}
pub fn join_markdown(output:&str,replacement:&str)->String {
    let left=output.trim_end_matches('\n');let right=replacement.trim_start_matches('\n');
    let newlines=(output.len()-left.len()).max(replacement.len()-right.len()).min(2);
    format!("{left}{}{right}","\n".repeat(newlines))
}
pub fn html_fragment_to_markdown(root:&dom_query::NodeRef<'_>)->String {
    fn ordered_start(value:&str)->f64 {
        let value=value.trim_matches(js_whitespace);if value.is_empty() {return 0.;}
        for (prefix,radix) in [("0x",16),("0X",16),("0b",2),("0B",2),("0o",8),("0O",8)] {
            if let Some(digits)=value.strip_prefix(prefix) {return if digits.is_empty() {f64::NAN} else {digits.chars().try_fold(0.,|number,character|character.to_digit(radix).map(|digit|number*f64::from(radix)+f64::from(digit))).unwrap_or(f64::NAN)};}
        }
        if value.contains("inf") {return f64::NAN;}
        value.parse().unwrap_or(f64::NAN)
    }
    fn process(parent:&dom_query::NodeRef<'_>,is_code:bool)->String {
        let mut output=String::new();
        for node in parent.children() {
            let name=node.node_name();let name=name.as_deref().unwrap_or("");let code=is_code||name=="code";
            let replacement=if node.is_text() {if code {node.text().into()} else {escape_markdown(&node.text())}}
                else if node.is_element() {replace(&node,code)} else {String::new()};
            output=join_markdown(&output,&replacement);
        }
        output
    }
    fn clean_attribute(value:&str)->String {
        static BREAKS:LazyLock<Regex>=LazyLock::new(||Regex::new(r"(?:\n+[\t-\r \u{00a0}\u{1680}\u{2000}-\u{200a}\u{2028}\u{2029}\u{202f}\u{205f}\u{3000}\u{feff}]*)+").expect("literal pattern"));
        BREAKS.replace_all(value,"\n").into_owned()
    }
    fn replace(node:&dom_query::NodeRef<'_>,is_code:bool)->String {
        let name=node.node_name();let name=name.as_deref().unwrap_or("");let block=markdown_block(name);
        let (leading,trailing)=markdown_flanking(node);let mut content=process(node,is_code);
        if !leading.is_empty() || !trailing.is_empty() {content=content.trim_matches(js_whitespace).into();}
        let replacement=if markdown_blank(node) {if block {"\n\n".into()} else {String::new()}}
        else {match name {
            "p"=>format!("\n\n{content}\n\n"),"br"=>"  \n".into(),
            "h1"|"h2"|"h3"|"h4"|"h5"|"h6"=>format!("\n\n{} {content}\n\n","#".repeat(usize::from(name.as_bytes()[1]-b'0'))),
            "blockquote"=>format!("\n\n{}\n\n",content.trim_matches('\n').split('\n').map(|line|format!("> {line}")).collect::<Vec<_>>().join("\n")),
            "ul"|"ol"=>{
                let nested=node.parent().is_some_and(|parent|parent.node_name().as_deref()==Some("li") && parent.element_children().last().is_some_and(|last|last.id==node.id));
                if nested {format!("\n{content}")} else {format!("\n\n{content}\n\n")}
            },
            "li"=>{
                let mut prefix="-   ".to_owned();
                if let Some(parent)=node.parent().filter(|parent|parent.node_name().as_deref()==Some("ol")) {
                    let index=parent.element_children().iter().position(|child|child.id==node.id).unwrap_or(0);
                    let start=parent.attr("start").filter(|value|!value.is_empty()).map_or(1.,|value|ordered_start(&value));
                    let number=start+index as f64;let label=if number==f64::INFINITY {"Infinity".into()} else if number==f64::NEG_INFINITY {"-Infinity".into()} else if number==0. {"0".into()} else {number.to_string()};prefix=format!("{label}.  ");
                }
                let paragraph=content.ends_with('\n');let content=format!("{}{}",content.trim_matches('\n'),if paragraph {"\n"} else {""});
                format!("{prefix}{}{}",content.replace('\n',&format!("\n{}"," ".repeat(prefix.encode_utf16().count()))),if node.next_sibling().is_some() {"\n"} else {""})
            },
            "pre" if node.children().first().is_some_and(|child|child.node_name().as_deref()==Some("code"))=>{
                let child=node.children()[0];let code=child.text();let class=child.attr("class").unwrap_or_default();
                let language=class.match_indices("language-").find_map(|(index,_)| {let value=class[index+9..].split(js_whitespace).next().unwrap_or("");(!value.is_empty()).then_some(value)}).unwrap_or("");
                let size=code.split('\n').map(|line|line.bytes().take_while(|byte|*byte==b'`').count()).filter(|size|*size>=3).max().map_or(3,|size|size+1);let fence="`".repeat(size);
                format!("\n\n{fence}{language}\n{}\n{fence}\n\n",code.strip_suffix('\n').unwrap_or(&code))
            },
            "hr"=>"\n\n---\n\n".into(),
            "a" if node.attr("href").is_some_and(|value|!value.is_empty())=>{
                let href=escape_link_destination(&node.attr("href").unwrap_or_default());let title=clean_attribute(&node.attr("title").unwrap_or_default()).replace('"',"\\\"");
                format!("[{content}]({href}{})",if title.is_empty() {String::new()} else {format!(" \"{title}\"")})
            },
            "em"|"i"=>if content.trim_matches(js_whitespace).is_empty() {String::new()} else {format!("*{content}*")},
            "strong"|"b"=>if content.trim_matches(js_whitespace).is_empty() {String::new()} else {format!("**{content}**")},
            "code" if !(node.parent().is_some_and(|parent|parent.node_name().as_deref()==Some("pre")) && node.prev_sibling().is_none() && node.next_sibling().is_none())=>{
                if content.is_empty() {String::new()} else {
                    let content=content.replace("\r\n"," ").replace(['\r','\n']," ");
                    let pad=content.starts_with('`')||content.ends_with('`')||(content.starts_with(' ')&&content.ends_with(' ')&&content.chars().any(|character|character!=' '));
                    let runs:Vec<_>=content.split(|character|character!='`').filter(|run|!run.is_empty()).map(str::len).collect();let mut size=1;while runs.contains(&size) {size+=1;}
                    let delimiter="`".repeat(size);let space=if pad {" "} else {""};format!("{delimiter}{space}{content}{space}{delimiter}")
                }
            },
            "img"=>{
                let src=escape_link_destination(&node.attr("src").unwrap_or_default());let alt=escape_markdown(&clean_attribute(&node.attr("alt").unwrap_or_default()));let title=clean_attribute(&node.attr("title").unwrap_or_default()).replace('"',"\\\"");
                if src.is_empty() {String::new()} else {format!("![{alt}]({src}{})",if title.is_empty() {String::new()} else {format!(" \"{title}\"")})}
            },
            "script"|"style"|"noscript"|"iframe"|"object"|"embed"|"meta"|"link"=>String::new(),
            _=>if block {format!("\n\n{content}\n\n")} else {content},
        }};
        format!("{leading}{replacement}{trailing}")
    }
    let cloned=root.tree.clone();let root=dom_query::NodeRef::new(root.id,&cloned);collapse_markdown_whitespace(&root);
    process(&root,false).trim_start_matches(['\t','\r','\n']).trim_end_matches(js_whitespace).into()
}
pub struct ReadableArticle { pub document:dom_query::Document,pub root:dom_query::NodeId,pub title:String,pub has_heading:bool }
pub fn extract_explicit_article(document:&dom_query::Document)->Option<ReadableArticle> {
    for selector in [".article_view",".tt_article_useless_p_margin",".entry-content",".contents_style",".post-content",".article-content",".content-article","#content .contents_style"] {
        let cloned=document.clone();
        let candidate=cloned.select_single(selector);let Some(root)=candidate.nodes().first() else {continue;};
        candidate.select("script, style, noscript, iframe, object, embed, meta, link, nav, aside, footer, .another_category, .area_related, .related, .revenue_unit_wrap, .adsbygoogle, .container_postbtn, .postbtn_like, .comments, .comment, .tagTrail, .sidebar").remove();
        if normalize_plain_text(&root.text()).encode_utf16().count()<30 {continue;}
        let title=select_preferred_title(document,&document.select("title").text());let has_heading=!candidate.select("h1, h2, h3, h4, h5, h6").is_empty();let root=root.id;
        return Some(ReadableArticle{document:cloned,root,title,has_heading});
    }
    None
}
pub fn select_preferred_title(document:&dom_query::Document,fallback:&str)->String {
    for selector in [".tit_post",".entry-title",".post-title",".article-title","h1"] {
        let title=normalize_plain_text(&document.select_single(selector).text());if !title.is_empty() {return title;}
    }
    normalize_plain_text(fallback)
}
pub fn html_fragment_to_plain_text(root:&dom_query::NodeRef<'_>)->String {
    fn visit(node:&dom_query::NodeRef<'_>,output:&mut String,is_root:bool) {
        if node.is_text() {output.push_str(&node.text());return;}
        let name=node.node_name();let name=name.as_deref().unwrap_or("");
        if !is_root && matches!(name,"script"|"style"|"noscript"|"iframe"|"object"|"embed"|"meta"|"link") {return;}
        if !is_root && name=="br" {output.push('\n');return;}
        let block=!is_root && matches!(name,"address"|"article"|"aside"|"blockquote"|"dd"|"div"|"dl"|"dt"|"figcaption"|"figure"|"footer"|"h1"|"h2"|"h3"|"h4"|"h5"|"h6"|"header"|"hr"|"li"|"main"|"nav"|"ol"|"p"|"pre"|"section"|"table"|"tbody"|"tfoot"|"thead"|"tr"|"ul");
        if block {output.push('\n');}
        for child in node.children() {visit(&child,output,false);}
        if block || (!is_root && matches!(name,"td"|"th")) {output.push('\n');}
    }
    let mut output=String::new();visit(root,&mut output,true);normalize_plain_text(&output)
}
pub fn normalize_plain_text(text:&str)->String {
    let spaces=WHITESPACE.replace_all(text," "); let before=BEFORE_NEWLINE.replace_all(&spaces,"\n"); let after=AFTER_NEWLINE.replace_all(&before,"\n"); NEWLINES.replace_all(&after,"\n\n").trim_matches(js_whitespace).into()
}
pub fn normalize_markdown(markdown:&str)->String {
    let normalized=markdown.replace("\r\n","\n").replace('\r',"\n"); let before=BEFORE_NEWLINE.replace_all(&normalized,"\n"); let after=AFTER_NEWLINE.replace_all(&before,"\n"); NEWLINES.replace_all(&after,"\n\n").trim_matches(js_whitespace).into()
}
fn js_whitespace(c:char)->bool { matches!(c,'\u{0009}'..='\u{000d}'|'\u{0020}'|'\u{00a0}'|'\u{1680}'|'\u{2000}'..='\u{200a}'|'\u{2028}'|'\u{2029}'|'\u{202f}'|'\u{205f}'|'\u{3000}'|'\u{feff}') }
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn reader_visibility_preserves_linkedom_style_case_and_last_nonempty_value() {
        for (style,visible) in [("display:none",false),("display:none;display:block",true),("display:none;display:",false),("DISPLAY:none",true),("display:none !important",true),("visibility:hidden",false)] {
            let document=dom_query::Document::from(format!("<div style='{style}'></div>"));assert_eq!(reader_probably_visible(&document.select("div").nodes()[0]),visible);
        }
        let document=dom_query::Document::from("<div id='hidden' hidden='false'></div><div id='aria' aria-hidden='true'></div><div id='math' aria-hidden='true' class='fallback-image'></div>");assert!(!reader_probably_visible(&document.select("#hidden").nodes()[0]));assert!(!reader_probably_visible(&document.select("#aria").nodes()[0]));assert!(reader_probably_visible(&document.select("#math").nodes()[0]));
    }
    #[test] fn reader_unlikely_candidates_preserve_source_exceptions_and_role_case() {
        let document=dom_query::Document::from("<div id='plain'></div><a id='anchor'></a><div id='role' role='navigation'></div><div id='case' role='Navigation'></div><code><span id='protected'></span></code>");let plain=document.select("#plain");assert!(reader_unlikely_candidate(&plain.nodes()[0],"sidebar"));assert!(!reader_unlikely_candidate(&plain.nodes()[0],"sidebar article"));assert!(!reader_unlikely_candidate(&document.select("#anchor").nodes()[0],"sidebar"));assert!(reader_unlikely_candidate(&document.select("#role").nodes()[0],"article"));assert!(!reader_unlikely_candidate(&document.select("#case").nodes()[0],"article"));assert!(!reader_unlikely_candidate(&document.select("#protected").nodes()[0],"sidebar"));
    }
    #[test] fn reader_byline_preserves_attribute_case_and_utf16_limit() {
        let document=dom_query::Document::from(format!("<p id='rel' rel='author'>Name</p><p id='case' rel='AUTHOR'>Name</p><p id='prop' itemprop='coauthor'>Name</p><p id='limit'>{}</p><p id='short'>{}</p>","😀".repeat(50),"😀".repeat(49)));
        assert!(reader_valid_byline(&document.select("#rel").nodes()[0],""));assert!(!reader_valid_byline(&document.select("#case").nodes()[0],""));assert!(reader_valid_byline(&document.select("#prop").nodes()[0],""));assert!(!reader_valid_byline(&document.select("#limit").nodes()[0],"AUTHOR"));assert!(reader_valid_byline(&document.select("#short").nodes()[0],"AUTHOR"));
    }
    #[test] fn reader_similarity_keeps_ascii_word_and_directional_distance_semantics() {
        assert_eq!(reader_text_similarity("same title","same title"),1.);assert_eq!(reader_text_similarity("한글","한글"),0.);assert_eq!(reader_text_similarity("a","a b"),1.-1./3.);assert_eq!(reader_text_similarity("a b","a"),1.);assert_eq!(reader_text_similarity("Title_name","title_name"),1.);
        let document=dom_query::Document::from("<h1>same title</h1><h3>same title</h3>");assert!(reader_header_duplicates_title(&document.select("h1").nodes()[0],"same title"));assert!(!reader_header_duplicates_title(&document.select("h3").nodes()[0],"same title"));
    }
    #[test] fn reader_depth_first_traversal_skips_text_and_survives_removal() {
        let document=dom_query::Document::from("<main id='root'>text<div id='removed'><span id='skip'>child</span></div><section id='next'><b id='last'>child</b></section></main>");let root=document.select("#root").nodes()[0];let removed=reader_next_node(root,false).unwrap();assert_eq!(removed.attr("id").as_deref(),Some("removed"));let next=reader_next_node(removed,true).unwrap();removed.remove_from_parent();assert_eq!(next.attr("id").as_deref(),Some("next"));let last=reader_next_node(next,false).unwrap();assert_eq!(last.attr("id").as_deref(),Some("last"));assert!(reader_next_node(last,false).is_none());
    }
    #[test] fn reader_document_preparation_removes_styles_and_preserves_font_attributes() {
        let document=dom_query::Document::from("<style>bad</style><div><font color='red'>text</font><br><br>after<style>bad</style></div>");reader_prepare_document(&document);
        assert!(document.select("style, font").is_empty());assert_eq!(document.select("span").attr("color").as_deref(),Some("red"));assert_eq!(document.select("span").text().as_ref(),"text");assert_eq!(document.select("p").text().as_ref(),"after");
    }
    #[test] fn reader_break_chains_create_paragraphs_until_next_chain_or_block() {
        let document=dom_query::Document::from("<div id='root'>foo<br>bar<br> <br><br>abc <em>inline</em> <br><br>last<section>block</section></div>");reader_replace_breaks(&document.select("#root").nodes()[0]);assert_eq!(document.select("#root").inner_html().as_ref(),"foo<br>bar<p> abc <em>inline</em></p><p>last</p><section>block</section>");
    }
    #[test] fn reader_break_chain_renames_parent_paragraph_and_drops_break_attributes() {
        let document=dom_query::Document::from("<p id='root'>before<br class='ignored'><br>after</p>");reader_replace_breaks(&document.select("#root").nodes()[0]);assert_eq!(document.select("#root").nodes()[0].node_name().as_deref(),Some("div"));assert_eq!(document.select("#root").inner_html().as_ref(),"before<p>after</p>");
    }
    #[test] fn reader_div_preparation_groups_inline_runs_around_blocks() {
        let document=dom_query::Document::from("<div id='root'>one <em>two</em> <br><section>block</section> three <strong>four</strong> </div>");let node=document.select("#root").nodes()[0];reader_prepare_div(node);
        assert_eq!(document.select("#root").inner_html().as_ref(),"<p>one <em>two</em></p><section>block</section><p> three <strong>four</strong> </p>");
    }
    #[test] fn reader_div_preparation_replaces_single_paragraph_only_below_density_limit() {
        let document=dom_query::Document::from("<div id='plain'>text</div><div id='linked'><p><a href='x'>linked</a></p></div>");let node=document.select("#plain").nodes()[0];let result=reader_prepare_div(node);assert_eq!(result.node_name().as_deref(),Some("p"));assert!(document.select("#plain").is_empty());assert_eq!(result.text().as_ref(),"text");
        reader_prepare_div(document.select("#linked").nodes()[0]);assert_eq!(document.select("#linked").nodes()[0].node_name().as_deref(),Some("div"));
    }
    #[test] fn reader_phrasing_recurses_only_for_conditional_inline_tags() {
        let document=dom_query::Document::from("<a id='inline'><strong>x</strong></a><a id='block'><div>x</div></a><span id='span'><div>x</div></span><a id='comment'><!--comment--></a>");
        assert!(reader_is_phrasing(&document.select("#inline").nodes()[0]));assert!(!reader_is_phrasing(&document.select("#block").nodes()[0]));assert!(reader_is_phrasing(&document.select("#span").nodes()[0]));assert!(!reader_is_phrasing(&document.select("#comment").nodes()[0]));assert!(reader_has_child_block(&document.select("#span").nodes()[0]));
    }
    #[test] fn reader_whitespace_treats_breaks_but_not_empty_elements_as_space() {
        let document=dom_query::Document::from("<div> \u{feff}<br><span></span><!--comment--></div>");let nodes=document.select("div").nodes()[0].children();assert!(reader_is_whitespace(&nodes[0]));assert!(reader_is_whitespace(&nodes[1]));assert!(!reader_is_whitespace(&nodes[2]));assert!(!reader_is_whitespace(&nodes[3]));
    }
    #[test] fn reader_single_tag_preserves_source_trailing_whitespace_content_rule() {
        let document=dom_query::Document::from("<div id='single'> <p>x</p> </div><div id='content'>text<p>x</p></div><div id='trailing'>text <p>x</p></div><div id='multiple'><p>x</p><p>y</p></div>");
        assert!(reader_has_single_tag(&document.select("#single").nodes()[0],"p"));assert!(!reader_has_single_tag(&document.select("#content").nodes()[0],"p"));assert!(reader_has_single_tag(&document.select("#trailing").nodes()[0],"p"));assert!(!reader_has_single_tag(&document.select("#multiple").nodes()[0],"p"));
    }
    #[test] fn reader_empty_element_uses_descendant_break_count_not_void_count() {
        let document=dom_query::Document::from("<div id='breaks'><br><hr></div><div id='image'><img src='x'></div><div id='nested'><span><br></span></div><div id='text'>x</div>");
        assert!(reader_element_without_content(&document.select("#breaks").nodes()[0]));assert!(!reader_element_without_content(&document.select("#image").nodes()[0]));assert!(reader_element_without_content(&document.select("#nested").nodes()[0]));assert!(!reader_element_without_content(&document.select("#text").nodes()[0]));
    }
    #[test] fn reader_text_density_counts_nested_selected_tags_separately() {
        let document=dom_query::Document::from("<div id='root'>a<span>😀<span>b</span></span><p>cc</p></div>");let root=document.select("#root");assert_eq!(reader_text_density(&root.nodes()[0],&["span"]),4./6.);assert_eq!(reader_text_density(&root.nodes()[0],&["div"]),0.);
        let empty=dom_query::Document::from("<div></div>");assert_eq!(reader_text_density(&empty.select("div").nodes()[0],&["span"]),0.);
    }
    #[test] fn reader_sibling_selection_preserves_exact_length_and_score_boundaries() {
        let document=dom_query::Document::from(format!("<div id='top' class='article'></div><div id='same' class='article'></div><p id='short'>Sentence.</p><p id='exact'>{}</p><p id='long'>{}</p><p id='linked'><a href='x'>Sentence.</a></p>","x".repeat(80),"x".repeat(81)));let top=document.select("#top");let top=&top.nodes()[0];
        assert!(reader_include_sibling(top,top,50.,None));assert!(reader_include_sibling(&document.select("#same").nodes()[0],top,50.,Some(0.)));assert!(!reader_include_sibling(&document.select("#same").nodes()[0],top,50.,None));
        assert!(reader_include_sibling(&document.select("#short").nodes()[0],top,50.,None));assert!(!reader_include_sibling(&document.select("#exact").nodes()[0],top,50.,None));assert!(reader_include_sibling(&document.select("#long").nodes()[0],top,50.,None));assert!(!reader_include_sibling(&document.select("#linked").nodes()[0],top,50.,None));
    }
    #[test] fn reader_top_candidates_keep_equal_score_encounter_order_and_limit() {
        let document=dom_query::Document::from("<div></div><div></div><div></div><div></div><div></div><div></div><div></div>");let nodes=document.select("div");
        let candidates:Vec<_>=nodes.nodes().iter().zip([1.,3.,3.,2.,0.,4.,3.]).map(|(node,score)|(node.id,score)).collect();let top=reader_top_candidates(&candidates);
        assert_eq!(top,vec![candidates[5],candidates[1],candidates[2],candidates[6],candidates[3]]);
    }
    #[test] fn reader_candidate_scores_propagate_in_encounter_order() {
        let document=dom_query::Document::from("<main><section><div><p>Paragraph contains enough text, with a comma.</p><p>tiny</p></div></section></main>");let paragraphs=document.select("p");let scores=score_reader_candidates(paragraphs.nodes(),false);
        let names:Vec<_>=scores.iter().map(|(id,_)|dom_query::NodeRef::new(*id,&document.tree).node_name().unwrap().to_string()).collect();assert_eq!(names,vec!["div","section","main","body"]);
        for ((_,actual),expected) in scores.iter().zip([8.,1.5,0.5,1./3.]) {assert!((actual-expected).abs()<1e-12);}
    }
    #[test] fn reader_candidate_scores_skip_short_text_and_scale_link_density() {
        let document=dom_query::Document::from("<div><p><a href='https://example.test'>All linked text sufficiently long to score.</a></p></div><section><p>short</p></section>");let scores=score_reader_candidates(document.select("p").nodes(),true);assert_eq!(scores[0].1,0.);assert!(scores[1].1>0.);assert_eq!(scores.len(),2);
    }
    #[test] fn reader_inner_text_collapses_runs_but_preserves_single_whitespace() {
        let document=dom_query::Document::from("<p> one\ttwo\nthree  four\u{00a0}\u{00a0}five </p>");let node=document.select("p");assert_eq!(reader_inner_text(&node.nodes()[0],true),"one\ttwo\nthree four five");assert_eq!(reader_inner_text(&node.nodes()[0],false),"one\ttwo\nthree  four\u{00a0}\u{00a0}five");
    }
    #[test] fn reader_density_weights_hash_links_and_utf16_lengths() {
        let document=dom_query::Document::from("<p>😀<a href='#local'>😀</a><a href='#'>xx</a></p>");assert!((reader_link_density(&document.select("p").nodes()[0])-2.6/6.).abs()<1e-12);let empty=dom_query::Document::from("<p></p>");assert_eq!(reader_link_density(&empty.select("p").nodes()[0]),0.);
    }
    #[test] fn reader_initial_scores_apply_class_and_id_independently() {
        let document=dom_query::Document::from("<div id='main' class='article comment'></div><h2 id='sidebar'></h2>");assert_eq!(reader_initial_score(&document.select("div").nodes()[0],true),30);assert_eq!(reader_initial_score(&document.select("h2").nodes()[0],true),-30);assert_eq!(reader_initial_score(&document.select("div").nodes()[0],false),5);
    }
    #[test] fn ordered_start_uses_javascript_number_coercion() {
        for (start,prefix) in [(" ","0.  "),("0x10","16.  "),("0b11","3.  "),("Infinity","Infinity.  "),("inf","NaN.  ")] {
            let document=dom_query::Document::from(format!("<ol start='{start}'><li>x</li></ol>"));assert!(html_fragment_to_markdown(&document.select("body").nodes()[0]).starts_with(prefix));
        }
    }
    #[test] fn fenced_language_skips_empty_first_class_match() {
        let document=dom_query::Document::from("<pre><code class='language- language-rust'>x</code></pre>");assert!(html_fragment_to_markdown(&document.select("body").nodes()[0]).starts_with("```rust\n"));
    }
    #[test] fn recursive_markdown_rules_preserve_source_tree() {
        let document=dom_query::Document::from("<h2>Heading</h2><p>one <em>two</em> <strong>three</strong><br>four</p><hr>");let before=document.html();let output=html_fragment_to_markdown(&document.select("body").nodes()[0]);
        assert!(output.starts_with("## Heading\n\n"));assert!(output.contains("*two* **three**  \nfour"));assert!(output.ends_with("\n\n---"));assert_eq!(document.html(),before);
    }
    #[test] fn recursive_markdown_lists_keep_ordered_start_and_nested_indentation() {
        let document=dom_query::Document::from("<ol start='3'><li>First</li><li><p>Second</p><ul><li>Nested</li></ul></li></ol>");let output=html_fragment_to_markdown(&document.select("body").nodes()[0]);assert!(output.starts_with("3.  First\n4.  Second"));assert!(output.contains("\n    -   Nested"));
    }
    #[test] fn recursive_markdown_code_expands_fence_and_pads_backtick_edges() {
        let document=dom_query::Document::from("<pre><code class='language-rust'>a\n```\nb\n</code></pre><p><code>`x`</code></p>");let output=html_fragment_to_markdown(&document.select("body").nodes()[0]);assert!(output.starts_with("````rust\na\n```\nb\n````"));assert!(output.ends_with("`` `x` ``"));
    }
    #[test] fn recursive_markdown_links_and_images_escape_attributes() {
        let document=dom_query::Document::from("<p><a href='a(b)' title='a &quot;b&quot;'>Link</a><img src='a b' alt='[x]'></p>");let output=html_fragment_to_markdown(&document.select("body").nodes()[0]);assert!(output.contains("[Link](a\\(b\\) \"a \\\"b\\\"\")"));assert!(output.contains("![\\[x\\]](<a b>)"));
    }
    #[test] fn markdown_blank_keeps_void_and_meaningful_descendants() {
        let document=dom_query::Document::from("<div id='blank'> \t </div><div id='image'><img src='x'></div><div id='link'><a href='x'></a></div>");assert!(markdown_blank(&document.select("#blank").nodes()[0]));assert!(!markdown_blank(&document.select("#image").nodes()[0]));assert!(!markdown_blank(&document.select("#link").nodes()[0]));
    }
    #[test] fn markdown_flanking_drops_only_neighbor_ascii_space() {
        let document=dom_query::Document::from("<div>left <em id='inline'> \u{00a0}middle\u{00a0} </em> right<p id='block'> block </p><span id='blank'> \t </span></div>");assert_eq!(markdown_flanking(&document.select("#inline").nodes()[0]),("\u{00a0}".into(),"\u{00a0}".into()));assert_eq!(markdown_flanking(&document.select("#block").nodes()[0]),(String::new(),String::new()));assert_eq!(markdown_flanking(&document.select("#blank").nodes()[0]),(" \t ".into(),String::new()));
    }
    #[test] fn markdown_whitespace_collapses_across_inline_nodes_and_block_edges() {
        let document=dom_query::Document::from("<div id='root'>  one <em>  two </em> three  <p> four\n five </p> six </div>");let root=document.select("#root");collapse_markdown_whitespace(&root.nodes()[0]);assert_eq!(root.inner_html().as_ref(),"one <em>two </em>three<p>four five</p>six");
    }
    #[test] fn markdown_whitespace_keeps_pre_and_void_spacing_and_removes_comments() {
        let document=dom_query::Document::from("<div id='root'>one <img src='x'> two<!--comment--><pre>  code\n  x </pre> end </div>");let root=document.select("#root");collapse_markdown_whitespace(&root.nodes()[0]);assert_eq!(root.text().as_ref(),"one  two  code\n  x end");assert!(!root.inner_html().contains("comment"));assert_eq!(root.select("pre").text().as_ref(),"  code\n  x ");
    }
    #[test] fn markdown_escape_order_preserves_text_node_semantics() {
        assert_eq!(escape_markdown("\\ * [x] _ `"),"\\\\ \\* \\[x\\] \\_ \\`");
        for (input,expected) in [("-item","\\-item"),("+ item","\\+ item"),("+item","+item"),("===","\\==="),("### heading","\\### heading"),("####### heading","####### heading"),("~~~code","\\~~~code"),("123. item","123\\. item"),("a\n- item","a\n- item")] {assert_eq!(escape_markdown(input),expected);}
    }
    #[test] fn inline_link_destinations_escape_delimiters_before_space_wrapping() {
        assert_eq!(escape_link_destination("https://example.test/a(b)"),"https://example.test/a\\(b\\)");assert_eq!(escape_link_destination("a <b>"),"<a \\<b\\>>");assert_eq!(escape_link_destination("a\tb"),"a\tb");
    }
    #[test] fn markdown_join_uses_maximum_boundary_newlines_capped_at_two() {
        for (left,right,expected) in [("a\n","\nb","a\nb"),("a\n\n\n","\nb","a\n\nb"),("a","\n\nb","a\n\nb"),("a "," b","a  b"),("","b","b")] {assert_eq!(join_markdown(left,right),expected);}
    }
    #[test] fn explicit_selector_priority_skips_missing_and_short_candidates() {
        let document=dom_query::Document::from("<div class='article_view'>short</div><div class='entry-content'>Chosen entry content has at least thirty characters.</div><div class='post-content'>Later post content is sufficiently long too.</div>");
        let article=extract_explicit_article(&document).unwrap();assert!(html_fragment_to_plain_text(&dom_query::NodeRef::new(article.root,&article.document.tree)).starts_with("Chosen entry"));
    }
    #[test] fn explicit_selection_removes_noise_and_keeps_original() {
        let document=dom_query::Document::from("<title>Fallback</title><h1 class='tit_post'> Preferred title </h1><div class='article_view'><p>Article text with enough characters for selection.</p><nav>nav noise</nav><div class='comments'>comment noise</div></div>");let original=document.html();
        let article=extract_explicit_article(&document).unwrap();let text=html_fragment_to_plain_text(&dom_query::NodeRef::new(article.root,&article.document.tree));assert!(!text.contains("noise"));assert_eq!(article.title,"Preferred title");assert!(!article.has_heading);assert_eq!(document.html(),original);
    }
    #[test] fn explicit_threshold_counts_utf16_and_heading_descendants() {
        let document=dom_query::Document::from(format!("<div class='post-content'><h2>{}</h2></div>","😀".repeat(15)));let article=extract_explicit_article(&document).unwrap();assert!(article.has_heading);
        assert!(extract_explicit_article(&dom_query::Document::from(format!("<div class='post-content'>{}</div>","😀".repeat(14)))).is_none());
    }
    #[test] fn title_selection_uses_first_match_then_next_selector_then_fallback() {
        let document=dom_query::Document::from("<div class='tit_post'> </div><div class='tit_post'>ignored second match</div><div class='entry-title'> Entry  title </div><h1>Heading</h1>");assert_eq!(select_preferred_title(&document,"fallback"),"Entry title");assert_eq!(select_preferred_title(&dom_query::Document::from("<p>body</p>")," fallback\t title "),"fallback title");
    }
    #[test] fn fragment_plain_text_preserves_source_block_and_cell_breaks() {
        let document=dom_query::Document::from("<div id='root'><p>one <strong>two</strong><br>three</p><table><tr><th>A</th><td>B</td></tr><tr><td>C</td><td>D</td></tr></table><p>last</p></div>");
        assert_eq!(html_fragment_to_plain_text(&document.select("#root").nodes()[0]),"one two\nthree\n\nA\nB\n\nC\nD\n\nlast");
    }
    #[test] fn fragment_plain_text_removes_noise_without_mutating_source() {
        let document=dom_query::Document::from("<div id='root'>before<script>unwanted</script><style>bad</style><iframe>bad</iframe><object>bad</object><embed><p>after &amp; end</p><!--not text--></div>");
        let before=document.select("#root").html();assert_eq!(html_fragment_to_plain_text(&document.select("#root").nodes()[0]),"before\nafter & end");assert_eq!(document.select("#root").html(),before);
    }
    #[test] fn fragment_plain_text_does_not_add_root_element_breaks() {
        let document=dom_query::Document::from("<div id='root'>one <span>two</span> three</div>");assert_eq!(html_fragment_to_plain_text(&document.select("#root").nodes()[0]),"one two three");
    }
    #[test] fn plain_normalization_keeps_two_newlines() { assert_eq!(normalize_plain_text("\u{feff} one\t two \n  three\n\n\n\n four \u{feff}"),"one two\nthree\n\nfour"); }
    #[test] fn markdown_normalizes_carriage_return_but_not_inline_space() { assert_eq!(normalize_markdown(" one   two\r\n  three\r\r\r four "),"one   two\nthree\n\nfour"); }
    #[test] fn javascript_does_not_trim_next_line_character() { assert_eq!(normalize_plain_text("\u{0085}x\u{0085}"),"\u{0085}x\u{0085}"); }
}
