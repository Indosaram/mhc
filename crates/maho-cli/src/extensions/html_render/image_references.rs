//! Port of `packages/coding-agent/src/extensions/html-render/image-references.ts` (senpi#6915).

use std::collections::HashSet;

pub const IMAGE_MIME_TYPES: [(&str, &str); 9] = [
    ("png", "image/png"),
    ("jpg", "image/jpeg"),
    ("jpeg", "image/jpeg"),
    ("gif", "image/gif"),
    ("webp", "image/webp"),
    ("avif", "image/avif"),
    ("svg", "image/svg+xml"),
    ("bmp", "image/bmp"),
    ("ico", "image/x-icon"),
];

pub fn image_mime_type_for_path(path: &str) -> Option<&'static str> {
    let dot = path.rfind('.')?;
    let ext = path[dot + 1..].to_ascii_lowercase();
    IMAGE_MIME_TYPES
        .iter()
        .find(|(candidate, _)| *candidate == ext)
        .map(|(_, mime)| *mime)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageReference {
    pub start: usize,
    pub end: usize,
    pub path: String,
}

#[derive(Clone, Debug)]
struct Attribute {
    name: String,
    start: usize,
    end: usize,
}

fn is_html_space(b: u8) -> bool {
    matches!(b, b'\t' | b'\n' | b'\x0c' | b'\r' | b' ')
}

fn is_attr_name_end(b: u8) -> bool {
    is_html_space(b) || matches!(b, b'/' | b'>' | b'=')
}

fn skip_space(html: &str, mut at: usize) -> usize {
    let bytes = html.as_bytes();
    while at < bytes.len() && is_html_space(bytes[at]) {
        at += 1;
    }
    at
}

fn end_after(html: &str, token: &str, from: usize) -> usize {
    html[from..]
        .find(token)
        .map_or(html.len(), |pos| from + pos + token.len())
}

fn comment_end(html: &str, from: usize) -> usize {
    if html[from..].starts_with('>') {
        return from + 1;
    }
    if html[from..].starts_with("->") {
        return from + 2;
    }
    end_after(html, "-->", from)
}

fn read_tag(html: &str, from: usize) -> Option<(Vec<Attribute>, usize)> {
    let bytes = html.as_bytes();
    let mut attributes = Vec::new();
    let mut names = HashSet::new();
    let mut at = from;
    while at < bytes.len() {
        let ch = bytes[at];
        if ch == b'>' {
            return Some((attributes, at + 1));
        }
        if ch == b'/' || is_html_space(ch) {
            at += 1;
            continue;
        }
        let name_start = at;
        at += 1;
        while at < bytes.len() && !is_attr_name_end(bytes[at]) {
            at += 1;
        }
        let name = html[name_start..at].to_ascii_lowercase();
        let equals = skip_space(html, at);
        let mut push_attr = |start: usize, end: usize| {
            if names.insert(name.clone()) {
                attributes.push(Attribute {
                    name: name.clone(),
                    start,
                    end,
                });
            }
        };
        if bytes.get(equals) != Some(&b'=') {
            push_attr(at, at);
            at = equals;
            continue;
        }
        let value = skip_space(html, equals + 1);
        let &quote = bytes.get(value)?;
        if quote == b'"' || quote == b'\'' {
            let q = quote as char;
            let rel_close = html[value + 1..].find(q)?;
            let close = value + 1 + rel_close;
            push_attr(value + 1, close);
            at = close + 1;
            continue;
        }
        at = value;
        while at < bytes.len() && !is_html_space(bytes[at]) && bytes[at] != b'>' {
            at += 1;
        }
        push_attr(value, at);
    }
    None
}

fn starts_with_absolute_path(s: &str) -> bool {
    let bytes = s.as_bytes();
    if bytes.first() == Some(&b'/') {
        return bytes.get(1) != Some(&b'/');
    }
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'/' || bytes[2] == b'\\')
}

fn is_valid_image_path(path: &str) -> bool {
    if !starts_with_absolute_path(path) || path.contains('\r') || path.contains('\n') {
        return false;
    }
    let Some(dot) = path.rfind('.') else {
        return false;
    };
    let prefix_chars = path[..dot].chars().count();
    let abs_len = if path.starts_with('/') { 1 } else { 3 };
    if prefix_chars > abs_len + 2048 {
        return false;
    }
    image_mime_type_for_path(path).is_some()
}

fn path_at(html: &str, start: usize, end: usize) -> Vec<ImageReference> {
    let bytes = html.as_bytes();
    let mut from = start;
    let mut to = end;
    while from < to && is_html_space(bytes[from]) {
        from += 1;
    }
    while to > from && is_html_space(bytes[to - 1]) {
        to -= 1;
    }
    let path = &html[from..to];
    if is_valid_image_path(path) {
        vec![ImageReference {
            start: from,
            end: to,
            path: path.to_owned(),
        }]
    } else {
        Vec::new()
    }
}

fn srcset_references(html: &str, start: usize, end: usize) -> Vec<ImageReference> {
    let bytes = html.as_bytes();
    let mut references = Vec::new();
    let mut at = start;
    while at < end {
        while at < end && (is_html_space(bytes[at]) || bytes[at] == b',') {
            at += 1;
        }
        let mut url_end = at;
        while url_end < end && !is_html_space(bytes[url_end]) {
            url_end += 1;
        }
        let mut trimmed = url_end;
        while trimmed > at && bytes[trimmed - 1] == b',' {
            trimmed -= 1;
        }
        references.extend(path_at(html, at, trimmed));
        let comma = if trimmed < url_end {
            Some(url_end - 1)
        } else {
            html[url_end..end].find(',').map(|p| url_end + p)
        };
        at = match comma {
            Some(pos) if pos < end => pos + 1,
            _ => end,
        };
    }
    references
}

fn scan_css_urls(html: &str, start: usize, end: usize) -> Vec<ImageReference> {
    let body = &html[start..end];
    let lower = body.to_ascii_lowercase();
    let mut refs = Vec::new();
    let mut at = 0usize;
    while let Some(rel) = lower[at..].find("url(") {
        let open = at + rel + 4;
        let inner = skip_space(body, open);
        let bytes = body.as_bytes();
        let Some(&first) = bytes.get(inner) else {
            break;
        };
        if first == b'"' || first == b'\'' {
            let quote = first as char;
            if let Some(close_rel) = body[inner + 1..].find(quote) {
                let p_start = inner + 1;
                let p_end = p_start + close_rel;
                let candidate = &body[p_start..p_end];
                let after = skip_space(body, p_end + 1);
                if bytes.get(after) == Some(&b')') && is_valid_image_path(candidate) {
                    refs.push(ImageReference {
                        start: start + p_start,
                        end: start + p_end,
                        path: candidate.to_owned(),
                    });
                    at = after + 1;
                    continue;
                }
            }
        } else {
            let mut p_end = inner;
            while p_end < bytes.len()
                && !is_html_space(bytes[p_end])
                && !matches!(bytes[p_end], b'"' | b'\'' | b'`' | b'(' | b')')
            {
                p_end += 1;
            }
            let after = skip_space(body, p_end);
            let candidate = &body[inner..p_end];
            if bytes.get(after) == Some(&b')') && is_valid_image_path(candidate) {
                refs.push(ImageReference {
                    start: start + inner,
                    end: start + p_end,
                    path: candidate.to_owned(),
                });
                at = after + 1;
                continue;
            }
        }
        at = open;
    }
    refs
}

fn scan_script_paths(html: &str, start: usize, end: usize) -> Vec<ImageReference> {
    let body = &html[start..end];
    let bytes = body.as_bytes();
    let mut refs = Vec::new();
    let mut at = 0usize;
    while at < bytes.len() {
        let b = bytes[at];
        if b == b'"' || b == b'\'' || b == b'`' {
            let quote = b as char;
            if let Some(rel) = body[at + 1..].find(quote) {
                let p_start = at + 1;
                let p_end = p_start + rel;
                let candidate = &body[p_start..p_end];
                if is_valid_image_path(candidate) {
                    refs.push(ImageReference {
                        start: start + p_start,
                        end: start + p_end,
                        path: candidate.to_owned(),
                    });
                    at = p_end + 1;
                    continue;
                }
            }
        } else if body[at..].len() >= 4 && body[at..at + 4].eq_ignore_ascii_case("url(") {
            let inner = skip_space(body, at + 4);
            let mut p_end = inner;
            while p_end < bytes.len()
                && !is_html_space(bytes[p_end])
                && !matches!(bytes[p_end], b'"' | b'\'' | b'`' | b'(' | b')')
            {
                p_end += 1;
            }
            let after = skip_space(body, p_end);
            let candidate = &body[inner..p_end];
            if bytes.get(after) == Some(&b')') && is_valid_image_path(candidate) {
                refs.push(ImageReference {
                    start: start + inner,
                    end: start + p_end,
                    path: candidate.to_owned(),
                });
                at = after + 1;
                continue;
            }
        }
        at += 1;
    }
    refs
}

fn is_url_attribute(name: &str) -> bool {
    matches!(
        name,
        "src" | "poster" | "href" | "xlink:href" | "data" | "background"
    )
}

fn is_raw_text_element(element: &str) -> bool {
    matches!(
        element,
        "script"
            | "style"
            | "textarea"
            | "title"
            | "xmp"
            | "noscript"
            | "iframe"
            | "noembed"
            | "noframes"
            | "plaintext"
    )
}

fn attribute_references(html: &str, attr: &Attribute) -> Vec<ImageReference> {
    if is_url_attribute(&attr.name) {
        return path_at(html, attr.start, attr.end);
    }
    if attr.name == "srcset" {
        return srcset_references(html, attr.start, attr.end);
    }
    if attr.name == "style" {
        return scan_css_urls(html, attr.start, attr.end);
    }
    if attr.name.starts_with("on") {
        return scan_script_paths(html, attr.start, attr.end);
    }
    Vec::new()
}

fn raw_text_end(lower: &str, element: &str, from: usize) -> usize {
    let token = format!("</{element}");
    let mut search = from;
    while let Some(rel) = lower[search..].find(&token) {
        let abs = search + rel;
        let next = lower.as_bytes().get(abs + token.len());
        if next.is_some_and(|b| is_html_space(*b) || matches!(*b, b'/' | b'>')) {
            return abs;
        }
        search = abs + 1;
    }
    lower.len()
}

/// The absolute image paths the page loads, in page order (`senpi#6915`).
pub fn find_local_images(html: &str) -> Vec<ImageReference> {
    let mut references = Vec::new();
    let lower = html.to_ascii_lowercase();
    let bytes = html.as_bytes();
    let mut foreign_depth = 0usize;
    let mut at = 0usize;
    while at < bytes.len() {
        let Some(rel) = html[at..].find('<') else {
            break;
        };
        let open = at + rel;
        let next = bytes.get(open + 1).copied();
        if html[open..].starts_with("<!--") {
            at = comment_end(html, open + 4);
            continue;
        }
        if foreign_depth > 0 && html[open..].starts_with("<![CDATA[") {
            at = end_after(html, "]]>", open + 9);
            continue;
        }
        if matches!(next, Some(b'!') | Some(b'?'))
            || (next == Some(b'/') && !bytes.get(open + 2).is_some_and(u8::is_ascii_alphabetic))
        {
            at = end_after(html, ">", (open + 2).min(bytes.len()));
            continue;
        }
        let closing = next == Some(b'/');
        let name_start = open + if closing { 2 } else { 1 };
        if !bytes.get(name_start).is_some_and(u8::is_ascii_alphabetic) {
            at = open + 1;
            continue;
        }
        let mut name_end = name_start + 1;
        while name_end < bytes.len()
            && !is_html_space(bytes[name_end])
            && !matches!(bytes[name_end], b'/' | b'>')
        {
            name_end += 1;
        }
        let element = html[name_start..name_end].to_ascii_lowercase();
        let Some((attrs, tag_end)) = read_tag(html, name_end) else {
            break;
        };
        at = tag_end;
        if element == "svg" || element == "math" {
            if closing {
                foreign_depth = foreign_depth.saturating_sub(1);
            } else if tag_end >= 2 && bytes[tag_end - 2] != b'/' {
                foreign_depth += 1;
            }
        }
        if closing {
            continue;
        }
        for attr in &attrs {
            references.extend(attribute_references(html, attr));
        }
        if !is_raw_text_element(&element) {
            continue;
        }
        let body_end = if element == "plaintext" {
            html.len()
        } else {
            raw_text_end(&lower, &element, at)
        };
        if element == "script" {
            references.extend(scan_script_paths(html, at, body_end));
        } else if element == "style" {
            references.extend(scan_css_urls(html, at, body_end));
        }
        at = body_end;
    }
    references
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_loading_contexts_and_ignores_text_comments_alt_and_xmp() {
        let html = r#"
            <!-- <img src="/tmp/comment.png"> -->
            <xmp><img src="/tmp/xmp.png"></xmp>
            <noscript><img src="/tmp/noscript.png"></noscript>
            <img src="/tmp/a.png" alt="/tmp/alt.png" srcset="/tmp/b.jpg 1x, /tmp/c.webp 2x">
            <div style="background:url('/tmp/bg.png')"></div>
            <style>.hero { background: url(/tmp/css.svg); }</style>
            <script>const p = "/tmp/js.gif";</script>
            <svg><![CDATA[<img src="/tmp/cdata.png">]]></svg>
        "#;
        let found: Vec<_> = find_local_images(html)
            .into_iter()
            .map(|r| r.path)
            .collect();
        assert_eq!(
            found,
            vec![
                "/tmp/a.png",
                "/tmp/b.jpg",
                "/tmp/c.webp",
                "/tmp/bg.png",
                "/tmp/css.svg",
                "/tmp/js.gif"
            ]
        );
    }
}
