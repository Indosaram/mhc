//! Port of senpi `websearch/websearch/html-document.ts` and `html-document.lazy.ts` (pin 903b8336).
//!
//! Upstream parses with linkedom behind a lazy import so CLI startup does not load the parser. The
//! native build links dom_query statically and parses only when a keyless engine returns a page, so
//! the lazy boundary has no separate module here.
pub use dom_query::Document;

/// `parseHtmlDocument`.
pub fn parse_html_document(html: &str) -> Document {
    Document::from(html)
}

/// `element.closest(selector)`: the node itself or its nearest matching ancestor.
pub fn closest<'a>(
    node: &dom_query::NodeRef<'a>,
    selector: &str,
) -> Option<dom_query::NodeRef<'a>> {
    if node.is(selector) {
        return Some(*node);
    }
    node.ancestors_it(None)
        .find(|ancestor| ancestor.is(selector))
}

/// `element.querySelector(selector)`: the first matching descendant.
pub fn query_selector<'a>(
    node: &dom_query::NodeRef<'a>,
    selector: &str,
) -> Option<dom_query::NodeRef<'a>> {
    dom_query::Selection::from(*node)
        .select(selector)
        .nodes()
        .first()
        .copied()
}

/// `element.textContent` as a string.
pub fn text_content(node: Option<&dom_query::NodeRef<'_>>) -> Option<String> {
    node.map(|node| node.text().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_results_page_for_selector_queries() {
        let document = parse_html_document(
            r#"<html><body><ul class="r"><li><a class="t" href="https://a.example/">A</a></li></ul></body></html>"#,
        );
        let link = document.select("ul.r > li a.t");
        assert_eq!(link.length(), 1);
        assert_eq!(link.attr("href").as_deref(), Some("https://a.example/"));
        assert_eq!(&*link.text(), "A");
    }

    #[test]
    fn closest_and_query_selector_walk_the_tree() {
        let document = parse_html_document(
            r#"<div class="c"><a href="/x"><h3>T</h3></a><p class="s">S</p></div>"#,
        );
        let heading = document.select("a h3").nodes()[0];
        let anchor = closest(&heading, "a").unwrap();
        assert_eq!(anchor.attr("href").as_deref(), Some("/x"));
        let container = closest(&heading, ".c").unwrap();
        assert_eq!(
            text_content(query_selector(&container, "p.s").as_ref()).as_deref(),
            Some("S")
        );
        assert!(closest(&heading, "section").is_none());
    }
}
