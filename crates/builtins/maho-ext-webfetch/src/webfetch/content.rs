use std::sync::LazyLock;
use regex::Regex;
static WHITESPACE:LazyLock<Regex>=LazyLock::new(||Regex::new(r"[\t\x0c\x0b \u{00a0}]+").expect("literal pattern"));
static BEFORE_NEWLINE:LazyLock<Regex>=LazyLock::new(||Regex::new(r"[ \t]+\n").expect("literal pattern"));
static AFTER_NEWLINE:LazyLock<Regex>=LazyLock::new(||Regex::new(r"\n[ \t]+").expect("literal pattern"));
static NEWLINES:LazyLock<Regex>=LazyLock::new(||Regex::new(r"\n{3,}").expect("literal pattern"));
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
