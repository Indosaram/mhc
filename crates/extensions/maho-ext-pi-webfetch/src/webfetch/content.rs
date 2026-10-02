pub struct ReadableArticle{pub title:String,pub content:String,pub has_heading:bool}
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
    let document=dom_query::Document::from(html);
    for selector in [".article_view",".tt_article_useless_p_margin",".entry-content",".contents_style",".post-content",".article-content",".content-article","#content .contents_style"]{
        let candidate=document.select(selector).first();if candidate.is_empty(){continue;}
        let cloned=dom_query::Document::fragment(candidate.html().as_ref());
        let root=cloned.select("html > *").first();
        root.select("script, style, noscript, iframe, object, embed, meta, link, nav, aside, footer, .another_category, .area_related, .related, .revenue_unit_wrap, .adsbygoogle, .container_postbtn, .postbtn_like, .comments, .comment, .tagTrail, .sidebar").remove();
        if normalize_plain_text(&root.text()).encode_utf16().count()<30{continue;}
        let mut title=String::new();for selector in [".tit_post",".entry-title",".post-title",".article-title","h1"]{title=normalize_plain_text(&document.select(selector).first().text());if !title.is_empty(){break;}}
        if title.is_empty(){title=normalize_plain_text(&document.select("title").first().text());}
        let content=root.inner_html().to_string();let has_heading=!root.select("h1,h2,h3,h4,h5,h6").is_empty();
        return Some(ReadableArticle{title,content,has_heading});
    }None
}
pub fn html_fragment_to_plain_text(html:&str)->String{
    let document=dom_query::Document::from(format!("<body>{html}</body>"));
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
