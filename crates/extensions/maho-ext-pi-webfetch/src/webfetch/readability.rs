//! Port of Mozilla Readability.js 0.6.0 (`@mozilla/readability`, Apache-2.0, Copyright (c) 2010 Arc90 Inc; notice in
//! THIRD_PARTY_NOTICES.md), as pi-webfetch `src/webfetch/content.ts` (pin 8a4743da) runs it:
//! `new Readability(document, { charThreshold: 80, keepClasses: false }).parse()`. Only the fields that consumer
//! reads are produced: `title`, `content` (the article root, whose `innerHTML` is upstream's serialized content)
//! and, through the root, `textContent`. `dir`, `lang`, `excerpt`, `siteName` and `publishedTime` are not computed.
//!
//! The algorithm is the one `maho-ext-webfetch` already carries for senpi's built-in webfetch (same pin of
//! Readability), run here over this crate's htmlparser2-shaped `dom_query` tree.
use super::content::js_whitespace;
use regex::Regex;
use std::sync::LazyLock;

/// What the consumer reads from `Readability.parse()`.
pub struct ParsedArticle<'a> {
    /// `article.title`.
    pub title: String,
    /// The detached article container: upstream `content` is its `innerHTML`, `textContent` its text.
    pub content: dom_query::NodeRef<'a>,
}

/// `new Readability(document, { charThreshold: 80, keepClasses: false }).parse()` over `document` (mutated, as
/// upstream mutates the JSDOM document it was handed). `url` is the document URL; the first `<base href>` sets
/// the base URI article links resolve against. `None` is upstream's `null` (nothing worth reading).
pub fn parse<'a>(document: &'a dom_query::Document, url: &str) -> Option<ParsedArticle<'a>> {
    let document_uri = url::Url::parse(url).map_or_else(|_| url.to_owned(), String::from);
    let base_uri = document
        .select("base[href]")
        .attr("href")
        .and_then(|href| resolve_web_url(&href, &document_uri))
        .map_or_else(|| document_uri.clone(), String::from);
    reader_unwrap_noscript_images(document);
    let (jsonld_title, jsonld_byline) = reader_jsonld(document);
    document.select("script,noscript").remove();
    reader_prepare_document(document);
    let (title, byline) =
        reader_metadata(document, jsonld_title.as_deref(), jsonld_byline.as_deref());
    let article = reader_grab_article(document, &title, byline.as_deref())?;
    reader_fix_relative_uris(&article, &base_uri, &document_uri);
    reader_simplify_nested(article);
    reader_clean_classes(&article, &["page"]);
    Some(ParsedArticle {
        title,
        content: article,
    })
}

/// `new URL(value, base).href`, `None` where that throws.
fn resolve_web_url(value: &str, base: &str) -> Option<url::Url> {
    url::Url::parse(value)
        .ok()
        .or_else(|| url::Url::parse(base).ok()?.join(value).ok())
}
pub fn reader_inner_text(node: &dom_query::NodeRef<'_>, normalize_spaces: bool) -> String {
    let text = node.text();
    let text = text.trim_matches(js_whitespace);
    if !normalize_spaces {
        return text.into();
    }
    let mut output = String::new();
    let mut whitespace = String::new();
    for character in text.chars() {
        if js_whitespace(character) {
            whitespace.push(character);
        } else {
            if whitespace.chars().count() >= 2 {
                output.push(' ');
            } else {
                output.push_str(&whitespace);
            }
            whitespace.clear();
            output.push(character);
        }
    }
    output.push_str(&whitespace);
    output
}
pub fn reader_link_density(node: &dom_query::NodeRef<'_>) -> f64 {
    let length = reader_inner_text(node, true).encode_utf16().count();
    if length == 0 {
        return 0.;
    }
    let links = dom_query::Selection::from(*node).select("a");
    let mut linked = 0.;
    for link in links.nodes() {
        let coefficient = if link
            .attr("href")
            .is_some_and(|href| href.starts_with('#') && href.len() > 1)
        {
            0.3
        } else {
            1.
        };
        linked += reader_inner_text(link, true).encode_utf16().count() as f64 * coefficient;
    }
    linked / length as f64
}
pub fn reader_text_density(node: &dom_query::NodeRef<'_>, tags: &[&str]) -> f64 {
    fn descendants_length(node: &dom_query::NodeRef<'_>, tags: &[&str]) -> usize {
        node.element_children()
            .iter()
            .map(|child| {
                let length = if child
                    .node_name()
                    .is_some_and(|name| tags.contains(&name.as_ref()))
                {
                    reader_inner_text(child, true).encode_utf16().count()
                } else {
                    0
                };
                length + descendants_length(child, tags)
            })
            .sum()
    }
    let length = reader_inner_text(node, true).encode_utf16().count();
    if length == 0 {
        return 0.;
    }
    descendants_length(node, tags) as f64 / length as f64
}
pub fn reader_initial_score(node: &dom_query::NodeRef<'_>, weight_classes: bool) -> i32 {
    static POSITIVE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new("(?i)article|body|content|entry|hentry|h-entry|main|page|pagination|post|text|blog|story").expect("literal pattern")
    });
    static NEGATIVE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new("(?i)-ad-|hidden|^hid$| hid$| hid |^hid |banner|combx|comment|com-|contact|footer|gdpr|masthead|media|meta|outbrain|promo|related|scroll|share|shoutbox|sidebar|skyscraper|sponsor|shopping|tags|widget").expect("literal pattern")
    });
    let name = node.node_name();
    let mut score = match name.as_deref() {
        Some("div") => 5,
        Some("pre" | "td" | "blockquote") => 3,
        Some("address" | "ol" | "ul" | "dl" | "dd" | "dt" | "li" | "form") => -3,
        Some("h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "th") => -5,
        _ => 0,
    };
    if weight_classes {
        for attribute in ["class", "id"] {
            if let Some(value) = node.attr(attribute) {
                if NEGATIVE.is_match(&value) {
                    score -= 25;
                }
                if POSITIVE.is_match(&value) {
                    score += 25;
                }
            }
        }
    }
    score
}
pub fn reader_has_single_tag(node: &dom_query::NodeRef<'_>, tag: &str) -> bool {
    let children = node.element_children();
    children.len() == 1
        && children[0].node_name().as_deref() == Some(tag)
        && !node.children().iter().any(|child| {
            child.is_text()
                && child
                    .text()
                    .chars()
                    .last()
                    .is_some_and(|character| !js_whitespace(character))
        })
}
pub fn reader_element_without_content(node: &dom_query::NodeRef<'_>) -> bool {
    let children = node.element_children();
    node.is_element()
        && node.text().trim_matches(js_whitespace).is_empty()
        && (children.is_empty()
            || children.len()
                == dom_query::Selection::from(*node)
                    .select("br, hr")
                    .nodes()
                    .len())
}
pub fn reader_has_child_block(node: &dom_query::NodeRef<'_>) -> bool {
    node.children().iter().any(|child| {
        child.node_name().is_some_and(|name| {
            matches!(
                name.as_ref(),
                "blockquote" | "dl" | "div" | "img" | "ol" | "p" | "pre" | "table" | "ul"
            )
        }) || reader_has_child_block(child)
    })
}
pub fn reader_is_phrasing(node: &dom_query::NodeRef<'_>) -> bool {
    if node.is_text() {
        return true;
    }
    let name = node.node_name();
    let name = name.as_deref().unwrap_or("");
    matches!(
        name,
        "abbr"
            | "audio"
            | "b"
            | "bdo"
            | "br"
            | "button"
            | "cite"
            | "code"
            | "data"
            | "datalist"
            | "dfn"
            | "em"
            | "embed"
            | "i"
            | "img"
            | "input"
            | "kbd"
            | "label"
            | "mark"
            | "math"
            | "meter"
            | "noscript"
            | "object"
            | "output"
            | "progress"
            | "q"
            | "ruby"
            | "samp"
            | "script"
            | "select"
            | "small"
            | "span"
            | "strong"
            | "sub"
            | "sup"
            | "textarea"
            | "time"
            | "var"
            | "wbr"
    ) || (matches!(name, "a" | "del" | "ins") && node.children().iter().all(reader_is_phrasing))
}
pub fn reader_is_whitespace(node: &dom_query::NodeRef<'_>) -> bool {
    (node.is_text() && node.text().trim_matches(js_whitespace).is_empty())
        || node.node_name().as_deref() == Some("br")
}
pub fn reader_prepare_div<'a>(node: dom_query::NodeRef<'a>) -> dom_query::NodeRef<'a> {
    let mut paragraph: Option<dom_query::NodeRef<'a>> = None;
    for child in node.children() {
        if reader_is_phrasing(&child) {
            if let Some(paragraph) = paragraph {
                paragraph.append_child(&child);
            } else if !reader_is_whitespace(&child) {
                child.before_html("<p></p>");
                let created = child.prev_sibling().expect("inserted paragraph");
                created.append_child(&child);
                paragraph = Some(created);
            }
        } else if let Some(current) = paragraph {
            while let Some(last) = current
                .children()
                .last()
                .copied()
                .filter(reader_is_whitespace)
            {
                last.remove_from_parent();
            }
            paragraph = None;
        }
    }
    if reader_has_single_tag(&node, "p") && reader_link_density(&node) < 0.25 {
        let paragraph = node.element_children()[0];
        node.replace_with(&paragraph);
        paragraph
    } else if !reader_has_child_block(&node) {
        node.rename("p");
        node
    } else {
        node
    }
}
pub fn reader_replace_breaks(root: &dom_query::NodeRef<'_>) {
    fn next_nonspace(mut node: Option<dom_query::NodeRef<'_>>) -> Option<dom_query::NodeRef<'_>> {
        while let Some(current) = node {
            if current.is_element() || !current.text().chars().all(js_whitespace) {
                break;
            }
            node = current.next_sibling();
        }
        node
    }
    let selection = dom_query::Selection::from(*root).select("br");
    for br in selection.nodes() {
        if br.parent().is_none() {
            continue;
        }
        let mut next = br.next_sibling();
        let mut replaced = false;
        while let Some(current) =
            next_nonspace(next).filter(|node| node.node_name().as_deref() == Some("br"))
        {
            replaced = true;
            next = current.next_sibling();
            current.remove_from_parent();
        }
        if !replaced {
            continue;
        }
        br.rename("p");
        br.remove_all_attrs();
        next = br.next_sibling();
        while let Some(current) = next {
            if current.node_name().as_deref() == Some("br")
                && next_nonspace(current.next_sibling())
                    .is_some_and(|node| node.node_name().as_deref() == Some("br"))
            {
                break;
            }
            if !reader_is_phrasing(&current) {
                break;
            }
            next = current.next_sibling();
            br.append_child(&current);
        }
        while let Some(last) = br.children().last().copied().filter(reader_is_whitespace) {
            last.remove_from_parent();
        }
        if let Some(parent) = br
            .parent()
            .filter(|parent| parent.node_name().as_deref() == Some("p"))
        {
            parent.rename("div");
        }
    }
}
pub fn reader_prepare_document(document: &dom_query::Document) {
    document.select("style").remove();
    if let Some(body) = document.select("body").nodes().first() {
        reader_replace_breaks(body);
    }
    for node in document.select("font").nodes() {
        node.rename("span");
    }
}
pub fn reader_next_node(
    mut node: dom_query::NodeRef<'_>,
    ignore_children: bool,
) -> Option<dom_query::NodeRef<'_>> {
    if !ignore_children && let Some(child) = node.first_element_child() {
        return Some(child);
    }
    loop {
        if let Some(sibling) = node.next_element_sibling() {
            return Some(sibling);
        }
        node = node.parent()?;
    }
}
pub fn reader_text_similarity(title: &str, heading: &str) -> f64 {
    let tokens = |text: &str| {
        text.to_lowercase()
            .split(|character: char| !(character.is_ascii_alphanumeric() || character == '_'))
            .filter(|token| !token.is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    let title = tokens(title);
    let heading = tokens(heading);
    if title.is_empty() || heading.is_empty() {
        return 0.;
    }
    let unique: Vec<_> = heading
        .iter()
        .filter(|token| !title.contains(token))
        .cloned()
        .collect();
    1. - unique.join(" ").len() as f64 / heading.join(" ").len() as f64
}
pub fn reader_header_duplicates_title(node: &dom_query::NodeRef<'_>, title: &str) -> bool {
    matches!(node.node_name().as_deref(), Some("h1" | "h2"))
        && reader_text_similarity(title, &reader_inner_text(node, false)) > 0.75
}
pub fn reader_valid_byline(node: &dom_query::NodeRef<'_>, match_string: &str) -> bool {
    static BYLINE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new("(?i)byline|author|dateline|writtenby|p-author").expect("literal pattern")
    });
    let length = node
        .text()
        .trim_matches(js_whitespace)
        .encode_utf16()
        .count();
    (node.attr("rel").as_deref() == Some("author")
        || node
            .attr("itemprop")
            .is_some_and(|value| value.contains("author"))
        || BYLINE.is_match(match_string))
        && length > 0
        && length < 100
}
pub fn reader_unlikely_candidate(node: &dom_query::NodeRef<'_>, match_string: &str) -> bool {
    static UNLIKELY: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new("(?i)-ad-|ai2html|banner|breadcrumbs|combx|comment|community|cover-wrap|disqus|extra|footer|gdpr|header|legends|menu|related|remark|replies|rss|shoutbox|sidebar|skyscraper|social|sponsor|supplemental|ad-break|agegate|pagination|pager|popup|yom-remote").expect("literal pattern")
    });
    static MAYBE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new("(?i)and|article|body|column|content|main|shadow").expect("literal pattern")
    });
    let mut ancestor = node.parent();
    let mut protected = false;
    for _ in 0..4 {
        let Some(parent) = ancestor else {
            break;
        };
        if matches!(parent.node_name().as_deref(), Some("table" | "code")) {
            protected = true;
            break;
        }
        ancestor = parent.parent();
    }
    let class_unlikely = UNLIKELY.is_match(match_string)
        && !MAYBE.is_match(match_string)
        && !protected
        && !matches!(node.node_name().as_deref(), Some("body" | "a"));
    class_unlikely
        || node.attr("role").is_some_and(|role| {
            matches!(
                role.as_ref(),
                "menu"
                    | "menubar"
                    | "complementary"
                    | "navigation"
                    | "alert"
                    | "alertdialog"
                    | "dialog"
            )
        })
}
pub fn reader_probably_visible(node: &dom_query::NodeRef<'_>) -> bool {
    let style = node.attr("style").unwrap_or_default();
    let mut display = "";
    let mut visibility = "";
    for rule in style.split(';') {
        if let Some((key, value)) = rule.split_once(':') {
            let key = key.trim_matches(js_whitespace);
            let value = value.trim_matches(js_whitespace);
            if value.is_empty() {
                continue;
            }
            match key {
                "display" => display = value,
                "visibility" => visibility = value,
                _ => {}
            }
        }
    }
    display != "none"
        && visibility != "hidden"
        && !node.has_attr("hidden")
        && (node.attr("aria-hidden").as_deref() != Some("true")
            || node
                .attr("class")
                .is_some_and(|class| class.contains("fallback-image")))
}
pub fn reader_prepare_nodes<'a>(
    root: dom_query::NodeRef<'a>,
    title: &str,
    strip_unlikely: bool,
    metadata_byline: Option<&str>,
) -> (Vec<dom_query::NodeRef<'a>>, Option<String>) {
    let mut elements = Vec::new();
    let mut byline = None;
    let mut remove_title = true;
    let mut current = Some(root);
    while let Some(mut node) = current {
        let match_string = format!(
            "{} {}",
            node.attr("class").unwrap_or_default(),
            node.attr("id").unwrap_or_default()
        );
        let mut remove = !reader_probably_visible(&node)
            || (node.attr("aria-modal").as_deref() == Some("true")
                && node.attr("role").as_deref() == Some("dialog"));
        if !remove
            && byline.is_none()
            && metadata_byline.is_none_or(str::is_empty)
            && reader_valid_byline(&node, &match_string)
        {
            let end = reader_next_node(node, true);
            let mut descendant = reader_next_node(node, false);
            let mut author = node;
            while let Some(child) = descendant {
                if end.is_some_and(|marker| marker.id == child.id) {
                    break;
                }
                if child
                    .attr("itemprop")
                    .is_some_and(|value| value.contains("name"))
                {
                    author = child;
                    break;
                }
                descendant = reader_next_node(child, false);
            }
            byline = Some(author.text().trim_matches(js_whitespace).to_owned());
            remove = true;
        }
        if !remove && remove_title && reader_header_duplicates_title(&node, title) {
            remove_title = false;
            remove = true;
        }
        if !remove && strip_unlikely && reader_unlikely_candidate(&node, &match_string) {
            remove = true;
        }
        let tag = node.node_name();
        if !remove
            && matches!(
                tag.as_deref(),
                Some("div" | "section" | "header" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6")
            )
            && reader_element_without_content(&node)
        {
            remove = true;
        }
        if remove {
            current = reader_next_node(node, true);
            node.remove_from_parent();
            continue;
        }
        if matches!(
            tag.as_deref(),
            Some("section" | "h2" | "h3" | "h4" | "h5" | "h6" | "p" | "td" | "pre")
        ) {
            elements.push(node);
        }
        if tag.as_deref() == Some("div") {
            node = reader_prepare_div(node);
            if node.node_name().as_deref() == Some("p") {
                elements.push(node);
            }
        }
        current = reader_next_node(node, false);
    }
    (elements, byline)
}
pub fn score_reader_candidates(
    elements: &[dom_query::NodeRef<'_>],
    weight_classes: bool,
) -> Vec<(dom_query::NodeId, f64)> {
    let mut candidates: Vec<(dom_query::NodeRef<'_>, f64)> = Vec::new();
    for element in elements {
        if element.parent().is_none_or(|parent| !parent.is_element()) {
            continue;
        }
        let text = reader_inner_text(element, true);
        let length = text.encode_utf16().count();
        if length < 25 {
            continue;
        }
        let commas = text
            .chars()
            .filter(|character| {
                matches!(
                    character,
                    ',' | '\u{060c}'
                        | '\u{fe50}'
                        | '\u{fe10}'
                        | '\u{fe11}'
                        | '\u{2e41}'
                        | '\u{2e34}'
                        | '\u{2e32}'
                        | '\u{ff0c}'
                )
            })
            .count();
        let content_score = 2. + commas as f64 + (length / 100).min(3) as f64;
        let mut ancestor = element.parent();
        for level in 0..5 {
            let Some(node) = ancestor else {
                break;
            };
            ancestor = node.parent();
            if !node.is_element() || node.parent().is_none_or(|parent| !parent.is_element()) {
                continue;
            }
            let index = if let Some(index) = candidates
                .iter()
                .position(|(candidate, _)| candidate.id == node.id)
            {
                index
            } else {
                candidates.push((node, f64::from(reader_initial_score(&node, weight_classes))));
                candidates.len() - 1
            };
            let divider = match level {
                0 => 1,
                1 => 2,
                _ => level * 3,
            };
            candidates[index].1 += content_score / divider as f64;
        }
    }
    for (node, score) in &mut candidates {
        *score *= 1. - reader_link_density(node);
    }
    candidates
        .into_iter()
        .map(|(node, score)| (node.id, score))
        .collect()
}
pub fn reader_top_candidates(
    candidates: &[(dom_query::NodeId, f64)],
) -> Vec<(dom_query::NodeId, f64)> {
    let mut top: Vec<(dom_query::NodeId, f64)> = Vec::new();
    for candidate in candidates {
        if let Some(index) = (0..5).find(|index| {
            top.get(*index)
                .is_none_or(|current| candidate.1 > current.1)
        }) {
            top.insert(index, *candidate);
            top.truncate(5);
        }
    }
    top
}
pub fn reader_refine_candidate<'a>(
    mut top: dom_query::NodeRef<'a>,
    ranked: &[(dom_query::NodeId, f64)],
    scores: &[(dom_query::NodeId, f64)],
    weight_classes: bool,
) -> dom_query::NodeRef<'a> {
    let score = |node: dom_query::NodeRef<'_>| {
        scores
            .iter()
            .find(|(id, _)| *id == node.id)
            .map(|(_, score)| *score)
    };
    let top_score =
        score(top).unwrap_or_else(|| f64::from(reader_initial_score(&top, weight_classes)));
    let alternatives: Vec<_> = ranked
        .iter()
        .skip(1)
        .filter(|(_, value)| *value / top_score >= 0.75)
        .map(|(id, _)| top.tree.get_unchecked(id))
        .collect();
    if alternatives.len() >= 3 {
        let mut ancestor = top.parent();
        while let Some(parent) = ancestor.filter(|node| node.node_name().as_deref() != Some("body"))
        {
            let containing = alternatives
                .iter()
                .filter(|alternative| {
                    let mut current = alternative.parent();
                    while let Some(node) = current {
                        if node.id == parent.id {
                            return true;
                        }
                        current = node.parent();
                    }
                    false
                })
                .count();
            if containing >= 3 {
                top = parent;
                break;
            }
            ancestor = parent.parent();
        }
    }
    let mut last =
        score(top).unwrap_or_else(|| f64::from(reader_initial_score(&top, weight_classes)));
    let threshold = last / 3.;
    let mut ancestor = top.parent();
    while let Some(parent) = ancestor.filter(|node| node.node_name().as_deref() != Some("body")) {
        if let Some(value) = score(parent) {
            if value < threshold {
                break;
            }
            if value > last {
                top = parent;
                break;
            }
            last = value;
        }
        ancestor = parent.parent();
    }
    while let Some(parent) = top.parent().filter(|parent| {
        parent.node_name().as_deref() != Some("body") && parent.element_children().len() == 1
    }) {
        top = parent;
    }
    top
}
pub fn reader_include_sibling(
    sibling: &dom_query::NodeRef<'_>,
    top: &dom_query::NodeRef<'_>,
    top_score: f64,
    sibling_score: Option<f64>,
) -> bool {
    if sibling.id == top.id {
        return true;
    }
    let class = top.attr("class").unwrap_or_default();
    let bonus = if !class.is_empty() && sibling.attr("class").unwrap_or_default() == class {
        top_score * 0.2
    } else {
        0.
    };
    if sibling_score.is_some_and(|score| score + bonus >= 10_f64.max(top_score * 0.2)) {
        return true;
    }
    if sibling.node_name().as_deref() != Some("p") {
        return false;
    }
    let text = reader_inner_text(sibling, true);
    let length = text.encode_utf16().count();
    let density = reader_link_density(sibling);
    (length > 80 && density < 0.25)
        || (length < 80
            && length > 0
            && density == 0.
            && (text.ends_with('.') || text.contains(". ")))
}
pub fn reader_gather_siblings<'a>(
    top: dom_query::NodeRef<'a>,
    top_score: f64,
    scores: &[(dom_query::NodeId, f64)],
) -> dom_query::NodeRef<'a> {
    let parent = top.parent().expect("candidate has parent");
    let siblings = parent.element_children();
    parent.append_html("<div></div>");
    let article = parent.children().last().copied().expect("inserted article");
    for sibling in siblings {
        let score = scores
            .iter()
            .find(|(id, _)| *id == sibling.id)
            .map(|(_, value)| *value);
        if reader_include_sibling(&sibling, &top, top_score, score) {
            if !matches!(
                sibling.node_name().as_deref(),
                Some("div" | "article" | "section" | "p" | "ol" | "ul")
            ) {
                sibling.rename("div");
            }
            article.append_child(&sibling);
        }
    }
    article.remove_from_parent();
    article
}
pub fn reader_clean_styles(node: &dom_query::NodeRef<'_>) {
    if node.node_name().as_deref() == Some("svg") {
        return;
    }
    for attribute in [
        "align",
        "background",
        "bgcolor",
        "border",
        "cellpadding",
        "cellspacing",
        "frame",
        "hspace",
        "rules",
        "style",
        "valign",
        "vspace",
    ] {
        node.remove_attr(attribute);
    }
    if matches!(
        node.node_name().as_deref(),
        Some("table" | "th" | "td" | "hr" | "pre")
    ) {
        node.remove_attr("width");
        node.remove_attr("height");
    }
    for child in node.element_children() {
        reader_clean_styles(&child);
    }
}
fn copy_reader_subtree<'a>(
    source: dom_query::NodeRef<'_>,
    target: &'a dom_query::Tree,
) -> dom_query::NodeRef<'a> {
    let data = source
        .tree
        .query_node(&source.id, |node| node.data.clone())
        .expect("existing source node");
    let id = target.create_node(data);
    let node = target.get_unchecked(&id);
    for child in source.children() {
        node.append_child(&copy_reader_subtree(child, target));
    }
    node
}
pub fn reader_grab_article<'a>(
    document: &'a dom_query::Document,
    title: &str,
    metadata_byline: Option<&str>,
) -> Option<dom_query::NodeRef<'a>> {
    let body = document.select("body").nodes().first().copied()?;
    let cache = document.tree.clone();
    let mut attempts = Vec::new();
    let mut byline = metadata_byline.map(str::to_owned);
    for (strip, weight, clean) in [
        (true, true, true),
        (false, true, true),
        (false, false, true),
        (false, false, false),
    ] {
        body.remove_children();
        for child in cache.get_unchecked(&body.id).children() {
            body.append_child(&copy_reader_subtree(child, &document.tree));
        }
        let root = document.select("html").nodes()[0];
        let (elements, found_byline) = reader_prepare_nodes(root, title, strip, byline.as_deref());
        if found_byline.is_some() {
            byline = found_byline;
        }
        let scores = score_reader_candidates(&elements, weight);
        let ranked = reader_top_candidates(&scores);
        let candidate = ranked.first().map(|(id, _)| root.tree.get_unchecked(id));
        let synthetic = candidate.is_none_or(|node| node.node_name().as_deref() == Some("body"));
        let top = if synthetic {
            let children = body.children();
            body.append_html("<div></div>");
            let container = body.children().last().copied().expect("inserted candidate");
            for child in children {
                container.append_child(&child);
            }
            container
        } else {
            reader_refine_candidate(
                candidate.expect("candidate present"),
                &ranked,
                &scores,
                weight,
            )
        };
        let score = scores.iter().find(|(id, _)| *id == top.id).map_or_else(
            || f64::from(reader_initial_score(&top, weight)),
            |(_, score)| *score,
        );
        let article = reader_gather_siblings(top, score, &scores);
        reader_prepare_article(&article, weight, clean);
        if synthetic {
            top.set_attr("id", "readability-page-1");
            top.set_attr("class", "page");
        } else {
            let children = article.children();
            article.append_html("<div id='readability-page-1' class='page'></div>");
            let page = article.children().last().copied().expect("inserted page");
            for child in children {
                page.append_child(&child);
            }
        }
        let length = reader_inner_text(&article, true).encode_utf16().count();
        if length >= 80 {
            return Some(article);
        }
        attempts.push((article, length));
    }
    attempts.sort_by_key(|(_, length)| std::cmp::Reverse(*length));
    attempts
        .into_iter()
        .find(|(_, length)| *length > 0)
        .map(|(article, _)| article)
}
pub fn reader_fix_relative_uris(root: &dom_query::NodeRef<'_>, base: &str, document_url: &str) {
    static SRCSET: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(\S+)(\s+[\d.]+[xw])?(\s*(?:,|$))").expect("literal pattern")
    });
    let absolute = |value: &str| {
        if base == document_url && value.starts_with('#') {
            value.to_owned()
        } else {
            resolve_web_url(value, base).map_or_else(|| value.to_owned(), String::from)
        }
    };
    let selection = dom_query::Selection::from(*root);
    for link in selection.select("a").nodes() {
        let Some(href) = link.attr("href").filter(|href| !href.is_empty()) else {
            continue;
        };
        if href.starts_with("javascript:") {
            let children = link.children();
            if children.len() == 1 && children[0].is_text() {
                link.replace_with(&children[0]);
            } else {
                link.rename("span");
                link.remove_all_attrs();
            }
        } else {
            link.set_attr("href", &absolute(&href));
        }
    }
    for media in selection
        .select("img,picture,figure,video,audio,source")
        .nodes()
    {
        for attribute in ["src", "poster"] {
            if let Some(value) = media.attr(attribute).filter(|value| !value.is_empty()) {
                media.set_attr(attribute, &absolute(&value));
            }
        }
        if let Some(value) = media.attr("srcset").filter(|value| !value.is_empty()) {
            let normalized = SRCSET.replace_all(&value, |capture: &regex::Captures<'_>| {
                format!(
                    "{}{}{}",
                    absolute(&capture[1]),
                    capture.get(2).map_or("", |part| part.as_str()),
                    &capture[3]
                )
            });
            media.set_attr("srcset", &normalized);
        }
    }
}
pub fn reader_jsonld(document: &dom_query::Document) -> (Option<String>, Option<String>) {
    static ARTICLE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^Article|AdvertiserContentArticle|NewsArticle|AnalysisNewsArticle|AskPublicNewsArticle|BackgroundNewsArticle|OpinionNewsArticle|ReportageNewsArticle|ReviewNewsArticle|Report|SatiricalArticle|ScholarlyArticle|MedicalScholarlyArticle|SocialMediaPosting|BlogPosting|LiveBlogPosting|DiscussionForumPosting|TechArticle|APIReference$").expect("literal pattern")
    });
    static CONTEXT: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^https?://schema\.org/?$").expect("literal pattern"));
    static CDATA: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^\s*<!\[CDATA\[|\]\]>\s*$").expect("literal pattern"));
    let article = |value: &serde_json::Value| {
        value
            .get("@type")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|kind| ARTICLE.is_match(kind))
    };
    for script in document.select("script").nodes() {
        if script.attr("type").as_deref() != Some("application/ld+json") {
            continue;
        }
        let text = script.text();
        let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&CDATA.replace_all(&text, ""))
        else {
            continue;
        };
        let mut selected = if let Some(array) = parsed.as_array() {
            let Some(value) = array.iter().find(|value| article(value)) else {
                continue;
            };
            value
        } else {
            &parsed
        };
        let context = selected.get("@context").and_then(|context| {
            context
                .as_str()
                .or_else(|| context.get("@vocab").and_then(serde_json::Value::as_str))
        });
        if !context.is_some_and(|context| CONTEXT.is_match(context)) {
            continue;
        }
        if selected
            .get("@type")
            .is_none_or(|kind| kind.is_null() || kind.as_str() == Some(""))
        {
            let Some(value) = selected
                .get("@graph")
                .and_then(serde_json::Value::as_array)
                .and_then(|graph| graph.iter().find(|value| article(value)))
            else {
                continue;
            };
            selected = value;
        }
        if !article(selected) {
            continue;
        }
        let name = selected.get("name").and_then(serde_json::Value::as_str);
        let headline = selected.get("headline").and_then(serde_json::Value::as_str);
        let title = match (name, headline) {
            (Some(name), Some(headline)) if name != headline => {
                let title = reader_article_title(document);
                Some(
                    if reader_text_similarity(headline, &title) > 0.75
                        && reader_text_similarity(name, &title) <= 0.75
                    {
                        headline
                    } else {
                        name
                    }
                    .to_owned(),
                )
            }
            (Some(name), _) => Some(name.trim_matches(js_whitespace).to_owned()),
            (_, Some(headline)) => Some(headline.trim_matches(js_whitespace).to_owned()),
            _ => None,
        };
        let author = selected.get("author");
        let byline = author
            .and_then(|author| author.get("name"))
            .and_then(serde_json::Value::as_str)
            .map(|name| name.trim_matches(js_whitespace).to_owned())
            .or_else(|| {
                let authors = author?.as_array()?;
                authors.first()?.get("name")?.as_str()?;
                Some(
                    authors
                        .iter()
                        .filter_map(|author| author.get("name").and_then(serde_json::Value::as_str))
                        .map(|name| name.trim_matches(js_whitespace))
                        .collect::<Vec<_>>()
                        .join(", "),
                )
            });
        return (title, byline);
    }
    (None, None)
}
pub fn reader_unescape_entities(text: &str) -> String {
    static NAMED: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"&(quot|amp|apos|lt|gt);").expect("literal pattern"));
    static NUMERIC: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)&#(?:x([0-9a-f]+)|([0-9]+));").expect("literal pattern"));
    let named = NAMED.replace_all(text, |capture: &regex::Captures<'_>| match &capture[1] {
        "quot" => "\"",
        "amp" => "&",
        "apos" => "'",
        "lt" => "<",
        _ => ">",
    });
    NUMERIC
        .replace_all(&named, |capture: &regex::Captures<'_>| {
            let value = if let Some(hex) = capture.get(1) {
                u32::from_str_radix(hex.as_str(), 16)
            } else {
                capture[2].parse()
            };
            value
                .ok()
                .filter(|value| *value != 0)
                .and_then(char::from_u32)
                .unwrap_or('\u{fffd}')
                .to_string()
        })
        .into_owned()
}
pub fn reader_metadata(
    document: &dom_query::Document,
    jsonld_title: Option<&str>,
    jsonld_byline: Option<&str>,
) -> (String, Option<String>) {
    static PROPERTY: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)\s*(article|dc|dcterm|og|twitter)\s*:\s*(author|creator|description|published_time|title|site_name)\s*").expect("literal pattern")
    });
    static NAME: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)^\s*(?:(dc|dcterm|og|twitter|parsely|weibo:(article|webpage))\s*[-\.:]\s*)?(author|creator|pub-date|description|title|site_name)\s*$").expect("literal pattern")
    });
    let mut values = std::collections::HashMap::new();
    for node in document.select("meta").nodes() {
        let Some(content) = node.attr("content").filter(|content| !content.is_empty()) else {
            continue;
        };
        let property = node.attr("property").unwrap_or_default();
        let key = if let Some(found) = PROPERTY.find(&property) {
            found
                .as_str()
                .to_lowercase()
                .chars()
                .filter(|character| !js_whitespace(*character))
                .collect::<String>()
        } else {
            let name = node.attr("name").unwrap_or_default();
            if !NAME.is_match(&name) {
                continue;
            }
            name.to_lowercase()
                .chars()
                .filter(|character| !js_whitespace(*character))
                .map(|character| if character == '.' { ':' } else { character })
                .collect()
        };
        values.insert(key, content.trim_matches(js_whitespace).to_owned());
    }
    let first = |keys: &[&str]| {
        keys.iter()
            .find_map(|key| values.get(*key).filter(|value| !value.is_empty()).cloned())
    };
    let title = jsonld_title
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            first(&[
                "dc:title",
                "dcterm:title",
                "og:title",
                "weibo:article:title",
                "weibo:webpage:title",
                "title",
                "twitter:title",
                "parsely-title",
            ])
        })
        .unwrap_or_else(|| reader_article_title(document));
    let byline = jsonld_byline
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .or_else(|| first(&["dc:creator", "dcterm:creator", "author", "parsely-author"]))
        .or_else(|| {
            values
                .get("article:author")
                .filter(|value| url::Url::parse(value).is_err())
                .cloned()
        });
    (
        reader_unescape_entities(&title),
        byline.map(|value| reader_unescape_entities(&value)),
    )
}
pub fn reader_unwrap_noscript_images(document: &dom_query::Document) {
    fn single_image(mut node: dom_query::NodeRef<'_>) -> bool {
        loop {
            if node.node_name().as_deref() == Some("img") {
                return true;
            }
            let children = node.element_children();
            if children.len() != 1 || !node.text().trim_matches(js_whitespace).is_empty() {
                return false;
            }
            node = children[0];
        }
    }
    static IMAGE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)\.(jpg|jpeg|png|webp)").expect("literal pattern"));
    for image in document.select("img").nodes() {
        if !image.attrs().iter().any(|attr| {
            matches!(
                attr.name.local.as_ref(),
                "src" | "srcset" | "data-src" | "data-srcset"
            ) || IMAGE.is_match(&attr.value)
        }) {
            image.remove_from_parent();
        }
    }
    for noscript in document.select("noscript").nodes() {
        if !single_image(*noscript) {
            continue;
        }
        let Some(previous) = noscript
            .prev_element_sibling()
            .filter(|node| single_image(*node))
        else {
            continue;
        };
        let old = if previous.node_name().as_deref() == Some("img") {
            previous
        } else {
            dom_query::Selection::from(previous).select("img").nodes()[0]
        };
        noscript.before_html("<div></div>");
        let temporary = noscript.prev_sibling().expect("inserted container");
        for child in noscript.children() {
            temporary.append_child(&copy_reader_subtree(child, &document.tree));
        }
        let image = dom_query::Selection::from(temporary).select("img").nodes()[0];
        for attr in old.attrs() {
            let name = attr.name.local.as_ref();
            if attr.value.is_empty()
                || (!matches!(name, "src" | "srcset") && !IMAGE.is_match(&attr.value))
            {
                continue;
            }
            if image.attr(name).is_some_and(|value| value == attr.value) {
                continue;
            }
            let target = if image.has_attr(name) {
                format!("data-old-{name}")
            } else {
                name.to_owned()
            };
            image.set_attr(&target, &attr.value);
        }
        let replacement = temporary
            .first_element_child()
            .expect("single image subtree");
        previous.replace_with(&replacement);
        temporary.remove_from_parent();
    }
}
pub fn reader_prepare_article(
    root: &dom_query::NodeRef<'_>,
    weight_classes: bool,
    clean_conditionally: bool,
) {
    static VIDEO: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)//(www\.)?((dailymotion|youtube|youtube-nocookie|player\.vimeo|v\.qq)\.com|(archive|upload\.wikimedia)\.org|player\.twitch\.tv)").expect("literal pattern")
    });
    static SHARE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)(\b|_)(share|sharedaddy)(\b|_)").expect("literal pattern")
    });
    reader_clean_styles(root);
    let selection = dom_query::Selection::from(*root);
    let tables: Vec<_> = selection
        .select("table")
        .nodes()
        .iter()
        .filter(|table| reader_data_table(table))
        .map(|table| table.id)
        .collect();
    reader_fix_lazy_images(root);
    if clean_conditionally {
        for tag in ["form", "fieldset"] {
            reader_clean_conditionally(root, tag, &tables, weight_classes);
        }
    }
    for tag in ["object", "embed", "footer", "link", "aside"] {
        for node in selection.select(tag).nodes().iter().rev() {
            let video = matches!(tag, "object" | "embed")
                && (node.attrs().iter().any(|attr| VIDEO.is_match(&attr.value))
                    || (tag == "object" && VIDEO.is_match(&node.inner_html())));
            if !video {
                node.remove_from_parent();
            }
        }
    }
    for top in root.element_children() {
        let end = reader_next_node(top, true);
        let mut current = reader_next_node(top, false);
        while let Some(node) = current {
            if end.is_some_and(|marker| marker.id == node.id) {
                break;
            }
            let name = format!(
                "{} {}",
                node.attr("class").unwrap_or_default(),
                node.attr("id").unwrap_or_default()
            );
            if SHARE.is_match(&name) && node.text().encode_utf16().count() < 500 {
                current = reader_next_node(node, true);
                node.remove_from_parent();
            } else {
                current = reader_next_node(node, false);
            }
        }
    }
    for tag in ["iframe", "input", "textarea", "select", "button"] {
        for node in selection.select(tag).nodes().iter().rev() {
            if tag != "iframe" || !node.attrs().iter().any(|attr| VIDEO.is_match(&attr.value)) {
                node.remove_from_parent();
            }
        }
    }
    for node in selection.select("h1,h2").nodes().iter().rev() {
        if reader_initial_score(node, weight_classes) - reader_initial_score(node, false) < 0 {
            node.remove_from_parent();
        }
    }
    if clean_conditionally {
        for tag in ["table", "ul", "div"] {
            reader_clean_conditionally(root, tag, &tables, weight_classes);
        }
    }
    reader_finish_article(root);
}
pub fn reader_simplify_nested(root: dom_query::NodeRef<'_>) {
    let mut current = Some(root);
    while let Some(node) = current {
        if node.parent().is_some()
            && matches!(node.node_name().as_deref(), Some("div" | "section"))
            && !node
                .attr("id")
                .is_some_and(|id| id.starts_with("readability"))
        {
            if reader_element_without_content(&node) {
                current = reader_next_node(node, true);
                node.remove_from_parent();
                continue;
            }
            if reader_has_single_tag(&node, "div") || reader_has_single_tag(&node, "section") {
                let child = node.element_children()[0];
                for attribute in node.attrs() {
                    child.set_attr(attribute.name.local.as_ref(), &attribute.value);
                }
                node.replace_with(&child);
                current = Some(child);
                continue;
            }
        }
        current = reader_next_node(node, false);
    }
}
pub fn reader_fix_lazy_images(root: &dom_query::NodeRef<'_>) {
    static BASE64: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)^data:\s*([^\s;,]+)\s*;\s*base64\s*,").expect("literal pattern")
    });
    static IMAGE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)\.(jpg|jpeg|png|webp)").expect("literal pattern"));
    static SRCSET: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\.(jpg|jpeg|png|webp)\s+\d").expect("literal pattern"));
    static SRC: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^\s*\S+\.(jpg|jpeg|png|webp)\S*\s*$").expect("literal pattern")
    });
    for node in dom_query::Selection::from(*root)
        .select("img,picture,figure")
        .nodes()
    {
        let src = node.attr("src").unwrap_or_default();
        if let Some(parts) = BASE64.captures(&src) {
            if parts
                .get(1)
                .is_some_and(|value| value.as_str() == "image/svg+xml")
            {
                continue;
            }
            if node.attrs().iter().any(|attribute| {
                attribute.name.local.as_ref() != "src" && IMAGE.is_match(&attribute.value)
            }) && src[parts.get(0).expect("full match").end()..]
                .encode_utf16()
                .count()
                < 133
            {
                node.remove_attr("src");
            }
        }
        if (node.attr("src").is_some_and(|value| !value.is_empty())
            || node
                .attr("srcset")
                .is_some_and(|value| !value.is_empty() && value.as_ref() != "null"))
            && !node
                .attr("class")
                .unwrap_or_default()
                .to_lowercase()
                .contains("lazy")
        {
            continue;
        }
        for attribute in node.attrs() {
            if matches!(attribute.name.local.as_ref(), "src" | "srcset" | "alt") {
                continue;
            }
            let target = if SRCSET.is_match(&attribute.value) {
                "srcset"
            } else if SRC.is_match(&attribute.value) {
                "src"
            } else {
                continue;
            };
            if matches!(node.node_name().as_deref(), Some("img" | "picture")) {
                node.set_attr(target, &attribute.value);
            } else if node.node_name().as_deref() == Some("figure")
                && dom_query::Selection::from(*node)
                    .select("img,picture")
                    .is_empty()
            {
                node.append_html("<img>");
                node.children()
                    .last()
                    .expect("inserted image")
                    .set_attr(target, &attribute.value);
            }
        }
    }
}
pub fn reader_clean_conditionally(
    root: &dom_query::NodeRef<'_>,
    tag: &str,
    data_tables: &[dom_query::NodeId],
    weight_classes: bool,
) {
    static VIDEO: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)//(www\.)?((dailymotion|youtube|youtube-nocookie|player\.vimeo|v\.qq)\.com|(archive|upload\.wikimedia)\.org|player\.twitch\.tv)").expect("literal pattern")
    });
    static SUSPICIOUS: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)^(ad(vertising|vertisement)?|pub(licité)?|werb(ung)?|广告|Реклама|Anuncio|(loading|正在加载|Загрузка|chargement|cargando)(…|\.\.\.)?)$").expect("literal pattern")
    });
    for node in dom_query::Selection::from(*root)
        .select(tag)
        .nodes()
        .iter()
        .rev()
    {
        let selection = dom_query::Selection::from(*node);
        let text = reader_inner_text(node, true);
        let length = text.encode_utf16().count() as f64;
        let list_length = selection
            .select("ul,ol")
            .nodes()
            .iter()
            .map(|list| reader_inner_text(list, true).encode_utf16().count())
            .sum::<usize>() as f64;
        let is_list = matches!(tag, "ul" | "ol") || list_length / length > 0.9;
        let mut protected = data_tables.contains(&node.id) && tag == "table";
        let mut figure = false;
        let mut ancestor = node.parent();
        let mut depth = 0;
        while let Some(parent) = ancestor {
            protected |= data_tables.contains(&parent.id)
                || (depth <= 3 && parent.node_name().as_deref() == Some("code"));
            figure |= depth <= 3 && parent.node_name().as_deref() == Some("figure");
            ancestor = parent.parent();
            depth += 1;
        }
        protected |= selection
            .select("table")
            .nodes()
            .iter()
            .any(|table| data_tables.contains(&table.id));
        if protected {
            continue;
        }
        let base = reader_initial_score(node, false);
        let weight = reader_initial_score(node, weight_classes) - base;
        if weight < 0 {
            node.remove_from_parent();
            continue;
        }
        if text.matches(',').count() >= 10 {
            continue;
        }
        let p = selection.select("p").length();
        let img = selection.select("img").length();
        let li = selection.select("li").length() as isize - 100;
        let input = selection.select("input").length();
        let embeds = selection.select("object,embed,iframe");
        if embeds.nodes().iter().any(|embed| {
            embed.attrs().iter().any(|attr| VIDEO.is_match(&attr.value))
                || (embed.node_name().as_deref() == Some("object")
                    && VIDEO.is_match(&embed.inner_html()))
        }) {
            continue;
        }
        if SUSPICIOUS.is_match(&text) {
            node.remove_from_parent();
            continue;
        }
        let density = reader_link_density(node);
        let heading_density = reader_text_density(node, &["h1", "h2", "h3", "h4", "h5", "h6"]);
        let text_density = reader_text_density(
            node,
            &[
                "span",
                "li",
                "td",
                "blockquote",
                "dl",
                "div",
                "img",
                "ol",
                "p",
                "pre",
                "table",
                "ul",
            ],
        );
        let remove = (!figure && img > 1 && (p as f64) / (img as f64) < 0.5)
            || (!is_list && li > p as isize)
            || input > p / 3
            || (!is_list
                && !figure
                && heading_density < 0.9
                && length < 25.
                && (img == 0 || img > 2)
                && density > 0.)
            || (!is_list && weight < 25 && density > 0.2)
            || (weight >= 25 && density > 0.5)
            || (embeds.length() == 1 && length < 75.)
            || embeds.length() > 1
            || (img == 0 && text_density == 0.);
        let image_list_exception = is_list
            && node
                .element_children()
                .iter()
                .all(|child| child.element_children().len() <= 1)
            && img == selection.select("li").length();
        if remove && !image_list_exception {
            node.remove_from_parent();
        }
    }
}
pub fn reader_data_table(table: &dom_query::NodeRef<'_>) -> bool {
    if table.attr("role").as_deref() == Some("presentation")
        || table.attr("datatable").as_deref() == Some("0")
    {
        return false;
    }
    if table.attr("summary").is_some_and(|value| !value.is_empty()) {
        return true;
    }
    let selection = dom_query::Selection::from(*table);
    if selection
        .select("caption")
        .nodes()
        .first()
        .is_some_and(|caption| !caption.children().is_empty())
        || !selection.select("col,colgroup,tfoot,thead,th").is_empty()
    {
        return true;
    }
    if !selection.select("table").is_empty() {
        return false;
    }
    let span = |value: Option<&str>| {
        let value = value.unwrap_or_default();
        let value = value.trim_start_matches(js_whitespace);
        let (sign, value) = if let Some(rest) = value.strip_prefix('-') {
            (-1., rest)
        } else {
            (1., value.strip_prefix('+').unwrap_or(value))
        };
        let digits: String = value.chars().take_while(char::is_ascii_digit).collect();
        let parsed = digits.parse::<f64>().unwrap_or(0.) * sign;
        if parsed == 0. { 1. } else { parsed }
    };
    let mut rows = 0.;
    let mut columns = 0_f64;
    for row in selection.select("tr").nodes() {
        rows += span(row.attr("rowspan").as_deref());
        let width = dom_query::Selection::from(*row)
            .select("td")
            .nodes()
            .iter()
            .map(|cell| span(cell.attr("colspan").as_deref()))
            .sum::<f64>();
        columns = columns.max(width);
    }
    if columns == 1. || rows == 1. {
        return false;
    }
    rows >= 10. || columns > 4. || rows * columns > 10.
}
pub fn reader_finish_article(root: &dom_query::NodeRef<'_>) {
    let selection = dom_query::Selection::from(*root);
    for heading in selection.select("h1").nodes() {
        heading.rename("h2");
    }
    for paragraph in selection.select("p").nodes().iter().rev() {
        if dom_query::Selection::from(*paragraph)
            .select("img,embed,object,iframe")
            .is_empty()
            && reader_inner_text(paragraph, false).is_empty()
        {
            paragraph.remove_from_parent();
        }
    }
    for br in selection.select("br").nodes() {
        let mut next = br.next_sibling();
        while let Some(node) = next {
            if node.is_element() || !node.text().chars().all(js_whitespace) {
                break;
            }
            next = node.next_sibling();
        }
        if next.is_some_and(|node| node.node_name().as_deref() == Some("p")) {
            br.remove_from_parent();
        }
    }
    for table in selection.select("table").nodes() {
        let tbody = if reader_has_single_tag(table, "tbody") {
            table.element_children()[0]
        } else {
            *table
        };
        if reader_has_single_tag(&tbody, "tr") {
            let row = tbody.element_children()[0];
            if reader_has_single_tag(&row, "td") {
                let cell = row.element_children()[0];
                cell.rename(if cell.children().iter().all(reader_is_phrasing) {
                    "p"
                } else {
                    "div"
                });
                table.replace_with(&cell);
            }
        }
    }
}
pub fn reader_clean_classes(node: &dom_query::NodeRef<'_>, preserved: &[&str]) {
    let classes = node.attr("class").unwrap_or_default();
    let classes = classes
        .split(js_whitespace)
        .filter(|class| preserved.contains(class))
        .collect::<Vec<_>>()
        .join(" ");
    if classes.is_empty() {
        node.remove_attr("class");
    } else {
        node.set_attr("class", &classes);
    }
    for child in node.element_children() {
        reader_clean_classes(&child, preserved);
    }
}
pub fn reader_article_title(document: &dom_query::Document) -> String {
    static WORDS: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"[\s\u{feff}]+").expect("literal pattern"));
    static SEPARATOR: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r" [|\-\\/>»] ").expect("literal pattern"));
    static FIRST: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^[^|\-\\/>»]*[|\-\\/>»]").expect("literal pattern"));
    static NORMALIZE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"[\s\u{feff}]{2,}").expect("literal pattern"));
    let original = document
        .select_single("head title")
        .text()
        .trim_matches(js_whitespace)
        .to_owned();
    let mut title = original.clone();
    let mut hierarchical = false;
    let count = |text: &str| WORDS.split(text).count();
    if let Some(separator) = SEPARATOR.find_iter(&title).last() {
        hierarchical = SEPARATOR
            .find_iter(&title)
            .any(|part| part.as_str().contains(['\\', '/', '>', '»']));
        title = original[..separator.start()].into();
        if count(&title) < 3 {
            title = FIRST.replace(&original, "").into_owned();
        }
    } else if title.contains(": ") {
        let matched = document.select("h1,h2").nodes().iter().any(|heading| {
            heading.text().trim_matches(js_whitespace) == title.trim_matches(js_whitespace)
        });
        if !matched {
            title = original[original.rfind(':').expect("colon present") + 1..].into();
            if count(&title) < 3 {
                title = original[original.find(':').expect("colon present") + 1..].into();
            } else if count(&original[..original.find(':').expect("colon present")]) > 5 {
                title = original.clone();
            }
        }
    } else if !(15..=150).contains(&title.encode_utf16().count()) {
        let headings = document.select("h1");
        if headings.length() == 1 {
            title = reader_inner_text(&headings.nodes()[0], true);
        }
    }
    title = NORMALIZE
        .replace_all(title.trim_matches(js_whitespace), " ")
        .into_owned();
    let words = count(&title);
    let stripped = original
        .chars()
        .filter(|character| !matches!(character, '|' | '-' | '\\' | '/' | '>' | '»'))
        .collect::<String>();
    if words <= 4 && (!hierarchical || words as isize != count(&stripped) as isize - 1) {
        title = original;
    }
    title
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reader_relative_uris_preserve_hashes_and_javascript_child_shapes() {
        let document = dom_query::Document::from(
            "<main><a href='#part'>hash</a><a href='javascript:x'>text</a><a href='javascript:x' class='drop'><b>bold</b> text</a><video poster='poster.png'><source src='movie.mp4'></video><img srcset='small.png 1x, large.png 2x'></main>",
        );
        reader_fix_relative_uris(
            &document.select("main").nodes()[0],
            "https://example.test/posts/a",
            "https://example.test/posts/a",
        );
        assert_eq!(document.select("a").length(), 1);
        assert_eq!(document.select("a").attr("href").as_deref(), Some("#part"));
        assert!(document.select("span").attr("class").is_none());
        assert_eq!(document.select("span b").text().as_ref(), "bold");
        assert_eq!(
            document.select("video").attr("poster").as_deref(),
            Some("https://example.test/posts/poster.png")
        );
        assert_eq!(
            document.select("img").attr("srcset").as_deref(),
            Some(
                "https://example.test/posts/small.png 1x, https://example.test/posts/large.png 2x"
            )
        );
    }
    #[test]
    fn reader_jsonld_selects_graph_headline_and_first_valid_metadata() {
        let document = dom_query::Document::from(
            r#"<title>A detailed headline with many words</title><script type='application/ld+json'>{"@context":"https://schema.org","@graph":[{"@type":"NewsArticle","name":"Site","headline":"A detailed headline with many words","author":[{"name":" One "},{"name":"Two"}]}]}</script><script type='application/ld+json'>{"@context":"https://schema.org","@type":"Article","name":"Later"}</script>"#,
        );
        assert_eq!(
            reader_jsonld(&document),
            (
                Some("A detailed headline with many words".into()),
                Some("One, Two".into())
            )
        );
    }
    #[test]
    fn reader_metadata_preserves_priority_first_property_and_entity_order() {
        let document = dom_query::Document::from(
            "<title>Fallback</title><meta property='og:title twitter:title' content='OG &amp;amp;#65;'><meta name='dc.title' content='DC'><meta property='article:author' content='https://example.test/author'>",
        );
        assert_eq!(reader_metadata(&document, None, None), ("DC".into(), None));
        assert_eq!(
            reader_metadata(&document, Some("JSON"), Some("Writer")),
            ("JSON".into(), Some("Writer".into()))
        );
        assert_eq!(
            reader_unescape_entities("&amp;#65; &#0; &#xD800; &AMP;"),
            "A � � &AMP;"
        );
    }
    #[test]
    fn reader_noscript_prepass_preserves_previous_image_sources() {
        let document = dom_query::Document::from(
            "<main><img id='empty'><img src='old.png' data-src='lazy.png'><noscript><img src='new.png'></noscript></main>",
        );
        reader_unwrap_noscript_images(&document);
        assert!(document.select("#empty").is_empty());
        assert_eq!(
            document.select("main > img").attr("src").as_deref(),
            Some("new.png")
        );
        assert_eq!(
            document
                .select("main > img")
                .attr("data-old-src")
                .as_deref(),
            Some("old.png")
        );
        assert_eq!(
            document.select("main > img").attr("data-src").as_deref(),
            Some("lazy.png")
        );
    }
    #[test]
    fn reader_grab_assembles_article_and_retries_short_unlikely_content() {
        let document = dom_query::Document::from(
            "<div class='sidebar'><p>A short fallback sentence.</p></div>",
        );
        reader_prepare_document(&document);
        let article = reader_grab_article(&document, "", None).unwrap();
        assert_eq!(
            reader_inner_text(&article, true),
            "A short fallback sentence."
        );
        assert_eq!(
            dom_query::Selection::from(article)
                .select("#readability-page-1")
                .length(),
            1
        );
    }
    #[test]
    fn reader_retry_keeps_source_byline_state_after_body_restore() {
        let document = dom_query::Document::from(
            "<title>Short</title><div class='author'>Writer</div><p>Short content.</p>",
        );
        reader_prepare_document(&document);
        let article = reader_grab_article(&document, "Short", None).unwrap();
        assert!(reader_inner_text(&article, true).contains("Writer"));
    }
    #[test]
    fn reader_article_preparation_preserves_video_and_top_share_candidate() {
        let document = dom_query::Document::from(
            "<main><section class='share'><p>Article</p><span class='share'>share buttons</span><iframe src='https://youtube.com/embed/x'></iframe><iframe src='https://invalid.test/x'></iframe><footer>footer</footer><h1 class='sidebar'>bad title</h1><h1>Good title</h1><img data-src='real.png'></section></main>",
        );
        reader_prepare_article(&document.select("main").nodes()[0], true, true);
        assert_eq!(document.select(".share").length(), 1);
        assert_eq!(document.select("iframe").length(), 1);
        assert!(document.select("footer,h1").is_empty());
        assert_eq!(document.select("h2").text().as_ref(), "Good title");
        assert_eq!(
            document.select("img").attr("src").as_deref(),
            Some("real.png")
        );
    }
    #[test]
    fn reader_nested_cleanup_copies_outer_attributes_and_preserves_readability_wrappers() {
        let document = dom_query::Document::from(
            "<main><div id='outer' class='outer'><section id='inner' class='inner'><p>text</p></section></div><div id='empty'></div><div id='readability-page-1'><div><p>page</p></div></div></main>",
        );
        reader_simplify_nested(document.select("main").nodes()[0]);
        assert!(document.select("#inner,#empty").is_empty());
        assert_eq!(
            document.select("#outer").nodes()[0].node_name().as_deref(),
            Some("section")
        );
        assert_eq!(
            document.select("#outer").attr("class").as_deref(),
            Some("outer")
        );
        assert_eq!(document.select("#readability-page-1 > div").length(), 1);
    }
    #[test]
    fn reader_conditional_cleanup_protects_tables_and_simple_image_lists() {
        let document = dom_query::Document::from(
            "<main><div id='links'><p><a href='x'>linked words only</a></p></div><div id='good'><p>Long readable text with sufficient density.</p></div><div id='protected' class='sidebar'><table summary='data'><tr><td>data</td></tr></table></div><ul><li><img src='x'></li><li><img src='y'></li></ul></main>",
        );
        let root = document.select("main").nodes()[0];
        let table = document.select("table").nodes()[0];
        reader_clean_conditionally(&root, "div", &[table.id], true);
        reader_clean_conditionally(&root, "ul", &[table.id], true);
        assert!(document.select("#links").is_empty());
        assert!(!document.select("#good,#protected,ul").is_empty());
        assert_eq!(document.select("#good,#protected,ul").length(), 3);
    }
    #[test]
    fn reader_lazy_images_replace_placeholders_and_create_figure_image() {
        let document = dom_query::Document::from(
            "<main><img id='placeholder' src='data:image/png;base64,a' data-src='real.png'><img id='svg' src='data:image/svg+xml;base64,a' data-src='real.png'><figure data-src='figure.jpg'></figure><picture data-srcset='small.png 1x, large.png 2x'></picture><img id='case' data-src='UPPER.PNG'></main>",
        );
        reader_fix_lazy_images(&document.select("main").nodes()[0]);
        assert_eq!(
            document.select("#placeholder").attr("src").as_deref(),
            Some("real.png")
        );
        assert_eq!(
            document.select("#svg").attr("src").as_deref(),
            Some("data:image/svg+xml;base64,a")
        );
        assert_eq!(
            document.select("figure img").attr("src").as_deref(),
            Some("figure.jpg")
        );
        assert_eq!(
            document.select("picture").attr("srcset").as_deref(),
            Some("small.png 1x, large.png 2x")
        );
        assert!(document.select("#case").attr("src").is_none());
    }
    #[test]
    fn reader_data_table_preserves_priority_and_decimal_span_prefixes() {
        for (html, expected) in [
            ("<table role='presentation'><th>x</th></table>", false),
            ("<table summary='data'><table></table></table>", true),
            (
                "<table><tr rowspan='  +3junk'><td colspan='4.5'>x</td></tr></table>",
                true,
            ),
            ("<table><tr rowspan='10'><td>x</td></tr></table>", false),
            ("<table><caption> </caption></table>", true),
        ] {
            let document = dom_query::Document::from(html);
            assert_eq!(
                reader_data_table(&document.select("table").nodes()[0]),
                expected
            );
        }
    }
    #[test]
    fn reader_finish_article_retains_media_and_unwraps_single_cell_tables() {
        let document = dom_query::Document::from(
            "<main><h1>Title</h1><p> </p><p><img src='x'></p><br> <p>text</p><table><tbody><tr><td id='cell'><em>inline</em></td></tr></tbody></table><table><tr><td><section>block</section></td></tr></table></main>",
        );
        reader_finish_article(&document.select("main").nodes()[0]);
        assert!(document.select("h1, br, table").is_empty());
        assert_eq!(document.select("h2").text().as_ref(), "Title");
        assert_eq!(document.select("p").length(), 3);
        assert_eq!(
            document.select("#cell").nodes()[0].node_name().as_deref(),
            Some("p")
        );
        assert_eq!(document.select("main > div").text().as_ref(), "block");
    }
    #[test]
    fn reader_title_preserves_short_fallback_and_separator_rules() {
        for (html, expected) in [
            (
                "<title>A long article title with words | Site</title>",
                "A long article title with words",
            ),
            ("<title>Short | Site</title>", "Short | Site"),
            (
                "<title>Site: A detailed article title with words</title>",
                "A detailed article title with words",
            ),
            (
                "<title>Short</title><h1>A detailed heading with enough words</h1>",
                "A detailed heading with enough words",
            ),
        ] {
            assert_eq!(
                reader_article_title(&dom_query::Document::from(html)),
                expected
            );
        }
    }
    #[test]
    fn reader_class_cleanup_preserves_duplicate_exact_names_and_svg_descendants() {
        let document = dom_query::Document::from(
            "<div class='page PAGE page other'><span class='keep page'>x</span><svg class='other'><g class='page'></g></svg></div>",
        );
        reader_clean_classes(&document.select("div").nodes()[0], &["page"]);
        assert_eq!(
            document.select("div").attr("class").as_deref(),
            Some("page page")
        );
        assert_eq!(
            document.select("span").attr("class").as_deref(),
            Some("page")
        );
        assert!(document.select("svg").attr("class").is_none());
        assert_eq!(document.select("g").attr("class").as_deref(), Some("page"));
    }
    #[test]
    fn reader_style_cleanup_skips_entire_svg_and_keeps_image_size() {
        let document = dom_query::Document::from(
            "<div style='color:red' align='left'><pre width='10' height='20' style='x'>code</pre><img width='10' height='20' style='x'><svg style='fill:red'><g style='x'></g></svg></div>",
        );
        reader_clean_styles(&document.select("div").nodes()[0]);
        assert!(document.select("div").attr("style").is_none());
        assert!(document.select("div").attr("align").is_none());
        assert!(document.select("pre").attr("width").is_none());
        assert!(document.select("pre").attr("height").is_none());
        assert_eq!(document.select("img").attr("width").as_deref(), Some("10"));
        assert!(document.select("img").attr("style").is_none());
        assert_eq!(
            document.select("svg").attr("style").as_deref(),
            Some("fill:red")
        );
        assert_eq!(document.select("g").attr("style").as_deref(), Some("x"));
    }
    #[test]
    fn reader_gather_moves_scored_and_sentence_siblings_in_original_order() {
        let document = dom_query::Document::from(
            "<main><p>Intro.</p><pre id='top' class='keep'>Code</pre><p>not a sentence</p><section id='scored'>Content</section></main>",
        );
        let top = document.select("#top").nodes()[0];
        let scored = document.select("#scored").nodes()[0];
        let article = reader_gather_siblings(top, 100., &[(scored.id, 20.)]);
        assert_eq!(
            article.inner_html().as_ref(),
            "<p>Intro.</p><div id=\"top\" class=\"keep\">Code</div><section id=\"scored\">Content</section>"
        );
        assert!(article.parent().is_none());
        assert_eq!(document.select("main").text().as_ref(), "not a sentence");
    }
    #[test]
    fn reader_refinement_joins_close_candidates_at_shared_parent() {
        let document = dom_query::Document::from(
            "<main><section><p id='a'>a</p><p id='b'>b</p><p id='c'>c</p><p id='d'>d</p></section><aside>x</aside></main>",
        );
        let nodes: Vec<_> = ["#a", "#b", "#c", "#d"]
            .iter()
            .map(|selector| document.select(selector).nodes()[0])
            .collect();
        let scores: Vec<_> = nodes
            .iter()
            .enumerate()
            .map(|(index, node)| (node.id, 100. - index as f64))
            .collect();
        let top = reader_refine_candidate(nodes[0], &scores, &scores, true);
        assert_eq!(top.node_name().as_deref(), Some("section"));
    }
    #[test]
    fn reader_preparation_filters_mutable_tree_and_extracts_nested_author() {
        let document = dom_query::Document::from(
            "<h1>Article title</h1><h1>Article title</h1><div class='author'>By <span itemprop='name'>Writer</span></div><div hidden>hidden</div><div class='sidebar'>noise</div><div id='text'>Content</div><section></section>",
        );
        let (elements, byline) = reader_prepare_nodes(
            document.select("html").nodes()[0],
            "Article title",
            true,
            None,
        );
        assert_eq!(byline.as_deref(), Some("Writer"));
        assert_eq!(document.select("h1").length(), 1);
        assert!(
            document
                .select(".author, [hidden], .sidebar, section")
                .is_empty()
        );
        assert_eq!(elements.len(), 1);
        assert_eq!(elements[0].text().as_ref(), "Content");
        assert_eq!(elements[0].node_name().as_deref(), Some("p"));
    }
    #[test]
    fn reader_visibility_preserves_linkedom_style_case_and_last_nonempty_value() {
        for (style, visible) in [
            ("display:none", false),
            ("display:none;display:block", true),
            ("display:none;display:", false),
            ("DISPLAY:none", true),
            ("display:none !important", true),
            ("visibility:hidden", false),
        ] {
            let document = dom_query::Document::from(format!("<div style='{style}'></div>"));
            assert_eq!(
                reader_probably_visible(&document.select("div").nodes()[0]),
                visible
            );
        }
        let document = dom_query::Document::from(
            "<div id='hidden' hidden='false'></div><div id='aria' aria-hidden='true'></div><div id='math' aria-hidden='true' class='fallback-image'></div>",
        );
        assert!(!reader_probably_visible(
            &document.select("#hidden").nodes()[0]
        ));
        assert!(!reader_probably_visible(
            &document.select("#aria").nodes()[0]
        ));
        assert!(reader_probably_visible(
            &document.select("#math").nodes()[0]
        ));
    }
    #[test]
    fn reader_unlikely_candidates_preserve_source_exceptions_and_role_case() {
        let document = dom_query::Document::from(
            "<div id='plain'></div><a id='anchor'></a><div id='role' role='navigation'></div><div id='case' role='Navigation'></div><code><span id='protected'></span></code>",
        );
        let plain = document.select("#plain");
        assert!(reader_unlikely_candidate(&plain.nodes()[0], "sidebar"));
        assert!(!reader_unlikely_candidate(
            &plain.nodes()[0],
            "sidebar article"
        ));
        assert!(!reader_unlikely_candidate(
            &document.select("#anchor").nodes()[0],
            "sidebar"
        ));
        assert!(reader_unlikely_candidate(
            &document.select("#role").nodes()[0],
            "article"
        ));
        assert!(!reader_unlikely_candidate(
            &document.select("#case").nodes()[0],
            "article"
        ));
        assert!(!reader_unlikely_candidate(
            &document.select("#protected").nodes()[0],
            "sidebar"
        ));
    }
    #[test]
    fn reader_byline_preserves_attribute_case_and_utf16_limit() {
        let document = dom_query::Document::from(format!(
            "<p id='rel' rel='author'>Name</p><p id='case' rel='AUTHOR'>Name</p><p id='prop' itemprop='coauthor'>Name</p><p id='limit'>{}</p><p id='short'>{}</p>",
            "😀".repeat(50),
            "😀".repeat(49)
        ));
        assert!(reader_valid_byline(&document.select("#rel").nodes()[0], ""));
        assert!(!reader_valid_byline(
            &document.select("#case").nodes()[0],
            ""
        ));
        assert!(reader_valid_byline(
            &document.select("#prop").nodes()[0],
            ""
        ));
        assert!(!reader_valid_byline(
            &document.select("#limit").nodes()[0],
            "AUTHOR"
        ));
        assert!(reader_valid_byline(
            &document.select("#short").nodes()[0],
            "AUTHOR"
        ));
    }
    #[test]
    fn reader_similarity_keeps_ascii_word_and_directional_distance_semantics() {
        assert_eq!(reader_text_similarity("same title", "same title"), 1.);
        assert_eq!(reader_text_similarity("한글", "한글"), 0.);
        assert_eq!(reader_text_similarity("a", "a b"), 1. - 1. / 3.);
        assert_eq!(reader_text_similarity("a b", "a"), 1.);
        assert_eq!(reader_text_similarity("Title_name", "title_name"), 1.);
        let document = dom_query::Document::from("<h1>same title</h1><h3>same title</h3>");
        assert!(reader_header_duplicates_title(
            &document.select("h1").nodes()[0],
            "same title"
        ));
        assert!(!reader_header_duplicates_title(
            &document.select("h3").nodes()[0],
            "same title"
        ));
    }
    #[test]
    fn reader_depth_first_traversal_skips_text_and_survives_removal() {
        let document = dom_query::Document::from(
            "<main id='root'>text<div id='removed'><span id='skip'>child</span></div><section id='next'><b id='last'>child</b></section></main>",
        );
        let root = document.select("#root").nodes()[0];
        let removed = reader_next_node(root, false).unwrap();
        assert_eq!(removed.attr("id").as_deref(), Some("removed"));
        let next = reader_next_node(removed, true).unwrap();
        removed.remove_from_parent();
        assert_eq!(next.attr("id").as_deref(), Some("next"));
        let last = reader_next_node(next, false).unwrap();
        assert_eq!(last.attr("id").as_deref(), Some("last"));
        assert!(reader_next_node(last, false).is_none());
    }
    #[test]
    fn reader_document_preparation_removes_styles_and_preserves_font_attributes() {
        let document = dom_query::Document::from(
            "<style>bad</style><div><font color='red'>text</font><br><br>after<style>bad</style></div>",
        );
        reader_prepare_document(&document);
        assert!(document.select("style, font").is_empty());
        assert_eq!(
            document.select("span").attr("color").as_deref(),
            Some("red")
        );
        assert_eq!(document.select("span").text().as_ref(), "text");
        assert_eq!(document.select("p").text().as_ref(), "after");
    }
    #[test]
    fn reader_break_chains_create_paragraphs_until_next_chain_or_block() {
        let document = dom_query::Document::from(
            "<div id='root'>foo<br>bar<br> <br><br>abc <em>inline</em> <br><br>last<section>block</section></div>",
        );
        reader_replace_breaks(&document.select("#root").nodes()[0]);
        assert_eq!(
            document.select("#root").inner_html().as_ref(),
            "foo<br>bar<p> abc <em>inline</em></p><p>last</p><section>block</section>"
        );
    }
    #[test]
    fn reader_break_chain_renames_parent_paragraph_and_drops_break_attributes() {
        let document =
            dom_query::Document::from("<p id='root'>before<br class='ignored'><br>after</p>");
        reader_replace_breaks(&document.select("#root").nodes()[0]);
        assert_eq!(
            document.select("#root").nodes()[0].node_name().as_deref(),
            Some("div")
        );
        assert_eq!(
            document.select("#root").inner_html().as_ref(),
            "before<p>after</p>"
        );
    }
    #[test]
    fn reader_div_preparation_groups_inline_runs_around_blocks() {
        let document = dom_query::Document::from(
            "<div id='root'>one <em>two</em> <br><section>block</section> three <strong>four</strong> </div>",
        );
        let node = document.select("#root").nodes()[0];
        reader_prepare_div(node);
        assert_eq!(
            document.select("#root").inner_html().as_ref(),
            "<p>one <em>two</em></p><section>block</section><p> three <strong>four</strong> </p>"
        );
    }
    #[test]
    fn reader_div_preparation_replaces_single_paragraph_only_below_density_limit() {
        let document = dom_query::Document::from(
            "<div id='plain'>text</div><div id='linked'><p><a href='x'>linked</a></p></div>",
        );
        let node = document.select("#plain").nodes()[0];
        let result = reader_prepare_div(node);
        assert_eq!(result.node_name().as_deref(), Some("p"));
        assert!(document.select("#plain").is_empty());
        assert_eq!(result.text().as_ref(), "text");
        reader_prepare_div(document.select("#linked").nodes()[0]);
        assert_eq!(
            document.select("#linked").nodes()[0].node_name().as_deref(),
            Some("div")
        );
    }
    #[test]
    fn reader_phrasing_recurses_only_for_conditional_inline_tags() {
        let document = dom_query::Document::from(
            "<a id='inline'><strong>x</strong></a><a id='block'><div>x</div></a><span id='span'><div>x</div></span><a id='comment'><!--comment--></a>",
        );
        assert!(reader_is_phrasing(&document.select("#inline").nodes()[0]));
        assert!(!reader_is_phrasing(&document.select("#block").nodes()[0]));
        assert!(reader_is_phrasing(&document.select("#span").nodes()[0]));
        assert!(!reader_is_phrasing(&document.select("#comment").nodes()[0]));
        assert!(reader_has_child_block(&document.select("#span").nodes()[0]));
    }
    #[test]
    fn reader_whitespace_treats_breaks_but_not_empty_elements_as_space() {
        let document =
            dom_query::Document::from("<div> \u{feff}<br><span></span><!--comment--></div>");
        let nodes = document.select("div").nodes()[0].children();
        assert!(reader_is_whitespace(&nodes[0]));
        assert!(reader_is_whitespace(&nodes[1]));
        assert!(!reader_is_whitespace(&nodes[2]));
        assert!(!reader_is_whitespace(&nodes[3]));
    }
    #[test]
    fn reader_single_tag_preserves_source_trailing_whitespace_content_rule() {
        let document = dom_query::Document::from(
            "<div id='single'> <p>x</p> </div><div id='content'>text<p>x</p></div><div id='trailing'>text <p>x</p></div><div id='multiple'><p>x</p><p>y</p></div>",
        );
        assert!(reader_has_single_tag(
            &document.select("#single").nodes()[0],
            "p"
        ));
        assert!(!reader_has_single_tag(
            &document.select("#content").nodes()[0],
            "p"
        ));
        assert!(reader_has_single_tag(
            &document.select("#trailing").nodes()[0],
            "p"
        ));
        assert!(!reader_has_single_tag(
            &document.select("#multiple").nodes()[0],
            "p"
        ));
    }
    #[test]
    fn reader_empty_element_uses_descendant_break_count_not_void_count() {
        let document = dom_query::Document::from(
            "<div id='breaks'><br><hr></div><div id='image'><img src='x'></div><div id='nested'><span><br></span></div><div id='text'>x</div>",
        );
        assert!(reader_element_without_content(
            &document.select("#breaks").nodes()[0]
        ));
        assert!(!reader_element_without_content(
            &document.select("#image").nodes()[0]
        ));
        assert!(reader_element_without_content(
            &document.select("#nested").nodes()[0]
        ));
        assert!(!reader_element_without_content(
            &document.select("#text").nodes()[0]
        ));
    }
    #[test]
    fn reader_text_density_counts_nested_selected_tags_separately() {
        let document = dom_query::Document::from(
            "<div id='root'>a<span>😀<span>b</span></span><p>cc</p></div>",
        );
        let root = document.select("#root");
        assert_eq!(reader_text_density(&root.nodes()[0], &["span"]), 4. / 6.);
        assert_eq!(reader_text_density(&root.nodes()[0], &["div"]), 0.);
        let empty = dom_query::Document::from("<div></div>");
        assert_eq!(
            reader_text_density(&empty.select("div").nodes()[0], &["span"]),
            0.
        );
    }
    #[test]
    fn reader_sibling_selection_preserves_exact_length_and_score_boundaries() {
        let document = dom_query::Document::from(format!(
            "<div id='top' class='article'></div><div id='same' class='article'></div><p id='short'>Sentence.</p><p id='exact'>{}</p><p id='long'>{}</p><p id='linked'><a href='x'>Sentence.</a></p>",
            "x".repeat(80),
            "x".repeat(81)
        ));
        let top = document.select("#top");
        let top = &top.nodes()[0];
        assert!(reader_include_sibling(top, top, 50., None));
        assert!(reader_include_sibling(
            &document.select("#same").nodes()[0],
            top,
            50.,
            Some(0.)
        ));
        assert!(!reader_include_sibling(
            &document.select("#same").nodes()[0],
            top,
            50.,
            None
        ));
        assert!(reader_include_sibling(
            &document.select("#short").nodes()[0],
            top,
            50.,
            None
        ));
        assert!(!reader_include_sibling(
            &document.select("#exact").nodes()[0],
            top,
            50.,
            None
        ));
        assert!(reader_include_sibling(
            &document.select("#long").nodes()[0],
            top,
            50.,
            None
        ));
        assert!(!reader_include_sibling(
            &document.select("#linked").nodes()[0],
            top,
            50.,
            None
        ));
    }
    #[test]
    fn reader_top_candidates_keep_equal_score_encounter_order_and_limit() {
        let document = dom_query::Document::from(
            "<div></div><div></div><div></div><div></div><div></div><div></div><div></div>",
        );
        let nodes = document.select("div");
        let candidates: Vec<_> = nodes
            .nodes()
            .iter()
            .zip([1., 3., 3., 2., 0., 4., 3.])
            .map(|(node, score)| (node.id, score))
            .collect();
        let top = reader_top_candidates(&candidates);
        assert_eq!(
            top,
            vec![
                candidates[5],
                candidates[1],
                candidates[2],
                candidates[6],
                candidates[3]
            ]
        );
    }
    #[test]
    fn reader_candidate_scores_propagate_in_encounter_order() {
        let document = dom_query::Document::from(
            "<main><section><div><p>Paragraph contains enough text, with a comma.</p><p>tiny</p></div></section></main>",
        );
        let paragraphs = document.select("p");
        let scores = score_reader_candidates(paragraphs.nodes(), false);
        let names: Vec<_> = scores
            .iter()
            .map(|(id, _)| {
                dom_query::NodeRef::new(*id, &document.tree)
                    .node_name()
                    .unwrap()
                    .to_string()
            })
            .collect();
        assert_eq!(names, vec!["div", "section", "main", "body"]);
        for ((_, actual), expected) in scores.iter().zip([8., 1.5, 0.5, 1. / 3.]) {
            assert!((actual - expected).abs() < 1e-12);
        }
    }
    #[test]
    fn reader_candidate_scores_skip_short_text_and_scale_link_density() {
        let document = dom_query::Document::from(
            "<div><p><a href='https://example.test'>All linked text sufficiently long to score.</a></p></div><section><p>short</p></section>",
        );
        let scores = score_reader_candidates(document.select("p").nodes(), true);
        assert_eq!(scores[0].1, 0.);
        assert!(scores[1].1 > 0.);
        assert_eq!(scores.len(), 2);
    }
    #[test]
    fn reader_inner_text_collapses_runs_but_preserves_single_whitespace() {
        let document =
            dom_query::Document::from("<p> one\ttwo\nthree  four\u{00a0}\u{00a0}five </p>");
        let node = document.select("p");
        assert_eq!(
            reader_inner_text(&node.nodes()[0], true),
            "one\ttwo\nthree four five"
        );
        assert_eq!(
            reader_inner_text(&node.nodes()[0], false),
            "one\ttwo\nthree  four\u{00a0}\u{00a0}five"
        );
    }
    #[test]
    fn reader_density_weights_hash_links_and_utf16_lengths() {
        let document =
            dom_query::Document::from("<p>😀<a href='#local'>😀</a><a href='#'>xx</a></p>");
        assert!((reader_link_density(&document.select("p").nodes()[0]) - 2.6 / 6.).abs() < 1e-12);
        let empty = dom_query::Document::from("<p></p>");
        assert_eq!(reader_link_density(&empty.select("p").nodes()[0]), 0.);
    }
    #[test]
    fn reader_initial_scores_apply_class_and_id_independently() {
        let document = dom_query::Document::from(
            "<div id='main' class='article comment'></div><h2 id='sidebar'></h2>",
        );
        assert_eq!(
            reader_initial_score(&document.select("div").nodes()[0], true),
            30
        );
        assert_eq!(
            reader_initial_score(&document.select("h2").nodes()[0], true),
            -30
        );
        assert_eq!(
            reader_initial_score(&document.select("div").nodes()[0], false),
            5
        );
    }
}
