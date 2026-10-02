use std::sync::LazyLock;
use regex::Regex;
static WHITESPACE:LazyLock<Regex>=LazyLock::new(||Regex::new(r"[\t\x0c\x0b \u{00a0}]+").expect("literal pattern"));
static BEFORE_NEWLINE:LazyLock<Regex>=LazyLock::new(||Regex::new(r"[ \t]+\n").expect("literal pattern"));
static AFTER_NEWLINE:LazyLock<Regex>=LazyLock::new(||Regex::new(r"\n[ \t]+").expect("literal pattern"));
static NEWLINES:LazyLock<Regex>=LazyLock::new(||Regex::new(r"\n{3,}").expect("literal pattern"));
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
