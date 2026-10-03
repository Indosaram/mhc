use maho_ext_pi_webfetch::webfetch::content::decode_html_entities;
#[test]fn markdown_base_compares_original_url_string_for_fragments(){use maho_ext_pi_webfetch::webfetch::content::html_to_markdown;let output=html_to_markdown("<base href='https://example.com/'><a href='#part'>Part</a>","https://example.com");assert!(output.contains("[Part](https://example.com/#part)"));}
#[test]fn markdown_resolves_image_sources_but_text_conversion_does_not_add_urls(){use maho_ext_pi_webfetch::webfetch::content::{html_to_markdown,html_to_text};let html="<p>Caption</p><img src='image.png' alt='Image'>";assert!(html_to_markdown(html,"https://example.com/docs/").contains("![Image](https://example.com/docs/image.png)"));assert_eq!(html_to_text(html,"https://example.com/docs/"),"Caption");}
#[test]fn markdown_normalizes_consumed_urls_and_unwraps_javascript(){use maho_ext_pi_webfetch::webfetch::content::html_to_markdown;let output=html_to_markdown("<a href='/page'>Page</a> <a href='javascript:alert(1)'>Text</a> <a href='#part'>Part</a>","https://example.com/root");assert!(output.contains("[Page](https://example.com/page)"));assert!(!output.contains("javascript:"));assert!(output.contains("Text"));assert!(output.contains("[Part](#part)"));}
#[test]fn markdown_base_href_changes_fragment_resolution(){use maho_ext_pi_webfetch::webfetch::content::html_to_markdown;let output=html_to_markdown("<base href='/docs/'><a href='page'>Page</a> <a href='#part'>Part</a>","https://example.com/root");assert!(output.contains("[Page](https://example.com/docs/page)"));assert!(output.contains("[Part](https://example.com/docs/#part)"));}
#[test]
fn pinned_consumed_rawtext_and_foreign_content_match(){
    use maho_ext_pi_webfetch::webfetch::content::{html_to_markdown,html_to_text};
    let cases:serde_json::Value=serde_json::from_str(include_str!("fixtures/pinned-rawtext-foreign-review.json")).expect("actual source");
    for case in cases.as_array().expect("cases"){
        let html=case["html"].as_str().expect("html");let url=case["url"].as_str().expect("url");
        assert_eq!(html_to_markdown(html,url),case["markdown"].as_str().expect("markdown"),"{}",case["name"]);
        assert_eq!(html_to_text(html,url),case["text"].as_str().expect("text"),"{}",case["name"]);
    }
}
#[test]
fn pinned_linkedom_independent_verifier_cases_match(){
    use maho_ext_pi_webfetch::webfetch::content::{html_to_markdown,html_to_text};
    let cases:serde_json::Value=serde_json::from_str(include_str!("fixtures/pinned-linkedom-verifier-delta.json")).expect("actual source");
    for case in cases.as_array().expect("cases"){
        let html=case["html"].as_str().expect("html");let url=case["url"].as_str().expect("url");
        assert_eq!(html_to_markdown(html,url),case["markdown"].as_str().expect("markdown"),"{}",case["name"]);
        assert_eq!(html_to_text(html,url),case["text"].as_str().expect("text"),"{}",case["name"]);
    }
}
#[test]
fn pinned_multiple_nested_and_adjacent_foster_tables_match() {
    use maho_ext_pi_webfetch::webfetch::content::{html_to_markdown, html_to_text};
    let cases:serde_json::Value=serde_json::from_str(include_str!("fixtures/pinned-foster-review-delta.json")).expect("pinned oracle");
    for case in cases.as_array().expect("cases"){
        let html=case["html"].as_str().expect("html");let url=case["url"].as_str().expect("url");
        assert_eq!(html_to_markdown(html,url),case["markdown"].as_str().expect("markdown"),"{}",case["name"]);
        assert_eq!(html_to_text(html,url),case["text"].as_str().expect("text"),"{}",case["name"]);
    }
}
#[test]
fn pinned_malformed_dom_and_utf16_article_threshold_match() {
    use maho_ext_pi_webfetch::webfetch::content::{html_to_markdown, html_to_text};
    let cases: serde_json::Value = serde_json::from_str(include_str!("fixtures/pinned-malformed-dom-delta.json")).expect("source-generated fixtures");
    for case in cases.as_array().expect("cases") {
        let html = case["html"].as_str().expect("html");
        let url = case["url"].as_str().expect("url");
        assert_eq!(html_to_markdown(html, url), case["markdown"].as_str().expect("markdown"), "{}", case["name"]);
        assert_eq!(html_to_text(html, url), case["text"].as_str().expect("text"), "{}", case["name"]);
    }
}
#[test]
fn pinned_explicit_selector_priority_noise_and_short_candidate_match() {
    use maho_ext_pi_webfetch::webfetch::content::{html_to_markdown, html_to_text};
    let cases: serde_json::Value = serde_json::from_str(include_str!("fixtures/pinned-explicit-selector-delta.json")).expect("source-generated fixtures");
    for case in cases.as_array().expect("cases") {
        let html = case["html"].as_str().expect("html");
        let url = case["url"].as_str().expect("url");
        assert_eq!(html_to_markdown(html, url), case["markdown"].as_str().expect("markdown"), "{}", case["name"]);
        assert_eq!(html_to_text(html, url), case["text"].as_str().expect("text"), "{}", case["name"]);
    }
}
#[test]fn markdown_traversal_matches_source_generated_fixtures(){use maho_ext_pi_webfetch::webfetch::content::html_fragment_to_markdown;let cases:serde_json::Value=serde_json::from_str(include_str!("../../../../.omo/evidence/task-39-fetch-markdown.json")).expect("generated fixtures");for case in cases.as_array().expect("cases"){let html=case["html"].as_str().expect("html");assert_eq!(html_fragment_to_markdown(html),case["markdown"].as_str().expect("markdown"),"{html}");}}
#[test]fn turndown_links_and_images_escape_attributes_without_reescaping_content(){use maho_ext_pi_webfetch::webfetch::content::{inline_link_markdown,image_markdown};assert_eq!(inline_link_markdown("*x*","a(b) c","a\"b"),"[*x*](<a\\(b\\) c> \"a\\\"b\")");assert_eq!(image_markdown("[x]","image",""),"![\\[x\\]](image)");assert_eq!(image_markdown("x","","title"),"");}
#[test]fn turndown_attributes_and_link_destinations_preserve_source_escaping(){use maho_ext_pi_webfetch::webfetch::content::{clean_markdown_attribute,escape_link_destination};assert_eq!(escape_link_destination("a(b)<c> d"),"<a\\(b\\)\\<c\\> d>");assert_eq!(escape_link_destination("a\tb"),"a\tb");assert_eq!(clean_markdown_attribute(" a \n\n \t\u{feff} b  \r c")," a \nb  \r c");}
#[test]fn turndown_whitespace_crosses_inline_nodes_but_preserves_pre_and_void_spaces(){use maho_ext_pi_webfetch::webfetch::content::collapse_markdown_whitespace;let doc=dom_query::Document::fragment("  a <em> b </em> c <img> d <p> e \n f </p><pre>  x\n y </pre><!--noise--> ");let root=doc.select("html");collapse_markdown_whitespace(root.nodes()[0]);assert_eq!(root.inner_html().as_ref(),"a <em>b </em>c <img> d<p>e f</p><pre>  x\n y </pre>");}
#[test]fn turndown_inline_code_uses_shortest_absent_backtick_run(){use maho_ext_pi_webfetch::webfetch::content::inline_code_markdown;assert_eq!(inline_code_markdown("a`b```c"),"``a`b```c``");assert_eq!(inline_code_markdown("`a``"),"``` `a`` ```");assert_eq!(inline_code_markdown(" a "),"`  a  `");assert_eq!(inline_code_markdown("   "),"`   `");assert_eq!(inline_code_markdown("a\r\nb\rc\nd"),"`a b c d`");assert_eq!(inline_code_markdown(""),"");}
#[test]fn turndown_join_takes_maximum_boundary_newlines_capped_at_two(){use maho_ext_pi_webfetch::webfetch::content::join_markdown;assert_eq!(join_markdown("a\n","\n\nb"),"a\n\nb");assert_eq!(join_markdown("\u{1f600}\n\n\n","\n\n\n\u{e9}"),"\u{1f600}\n\n\u{e9}");assert_eq!(join_markdown("a "," b"),"a  b");assert_eq!(join_markdown("","\n\na"),"\n\na");}
#[test]fn turndown_escaping_is_ordered_and_anchored(){use maho_ext_pi_webfetch::webfetch::content::escape_markdown;assert_eq!(escape_markdown("- *a* [b]_c`\\"),"\\- \\*a\\* \\[b\\]\\_c\\`\\\\");assert_eq!(escape_markdown("123. item"),"123\\. item");assert_eq!(escape_markdown("###### title"),"\\###### title");assert_eq!(escape_markdown("####### title"),"####### title");assert_eq!(escape_markdown("a\n- next"),"a\n- next");assert_eq!(escape_markdown("~~~code"),"\\~~~code");assert_eq!(escape_markdown("=== x"),"\\=== x");}
#[test]fn explicit_article_preserves_noisy_root_and_only_removes_descendants(){use maho_ext_pi_webfetch::webfetch::content::extract_explicit_article;let article=extract_explicit_article("<aside class='entry-content'><p>Article content long enough to qualify here.</p><aside>noise</aside></aside>").expect("article");assert_eq!(article.content,"<p>Article content long enough to qualify here.</p>");}
#[test]fn explicit_article_selector_priority_and_noise_removal(){use maho_ext_pi_webfetch::webfetch::content::extract_explicit_article;let article=extract_explicit_article("<title>Fallback</title><h1>Preferred</h1><div class='entry-content'>Second article content long enough to qualify.</div><div class='article_view'><p>First article content long enough to qualify.</p><aside>noise</aside></div>").expect("article");assert_eq!(article.title,"Preferred");assert!(!article.has_heading);assert_eq!(article.content,"<p>First article content long enough to qualify.</p>");}
#[test]fn explicit_article_threshold_counts_utf16(){use maho_ext_pi_webfetch::webfetch::content::extract_explicit_article;let html=format!("<div class='entry-content'>{}</div>","\u{1f600}".repeat(15));assert!(extract_explicit_article(&html).is_some());assert!(extract_explicit_article("<div class='entry-content'>short</div>").is_none());}
#[test]fn html_fragment_dom_removes_noise_and_separates_blocks(){use maho_ext_pi_webfetch::webfetch::content::html_fragment_to_plain_text;assert_eq!(html_fragment_to_plain_text("<script>noise</script><p>a&nbsp;b<br>c</p><div>d</div><table><tr><td>e</td><td>f</td></tr></table>"),"a b\nc\n\nd\n\ne\nf");}
#[test]fn html_fragment_dom_decodes_full_named_entities(){use maho_ext_pi_webfetch::webfetch::content::html_fragment_to_plain_text;assert_eq!(html_fragment_to_plain_text("<p>&copy; &eacute; &amp;lt;</p>"),"\u{a9} \u{e9} &lt;");}
#[test]fn named_entities_decode_without_double_decoding(){assert_eq!(decode_html_entities("&amp;lt; &AMP; &unknown;"),"&lt; & &unknown;");}
#[test]fn numeric_entities_decode_unicode(){assert_eq!(decode_html_entities("&#128512; &#x1f600;"),"😀 😀");}
#[test]fn malformed_entities_remain_literal(){assert_eq!(decode_html_entities("&bad&copy; &#X41; &unterminated"),"&bad&copy; &#X41; &unterminated");}
#[test]fn out_of_range_codepoint_is_empty(){assert_eq!(decode_html_entities("before&#1114112;after"),"beforeafter");}
#[test]fn plain_text_collapses_horizontal_whitespace_and_preserves_cr(){use maho_ext_pi_webfetch::webfetch::content::normalize_plain_text;assert_eq!(normalize_plain_text("\u{feff} a\t\u{000b}\u{00a0}b  \n  c\n\n\n\n d\r\ne "),"a b\nc\n\nd\r\ne");}
#[test]fn markdown_normalizes_cr_without_collapsing_inline_spacing(){use maho_ext_pi_webfetch::webfetch::content::normalize_markdown;assert_eq!(normalize_markdown("  # Title\r\n\r\n\r\n  a  b \t\r c\t "),"# Title\n\na  b\nc");}
