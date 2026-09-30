//! Port of senpi packages/ai/src/tool-call-middleware/protocols/anthropic-xml/xml-entities.ts.

pub fn encode_xml_text(value: &str) -> String {
    value.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

fn encode_xml_line_breaks(value: &str) -> String {
    value.replace('\r', "&#13;").replace('\n', "&#10;")
}

pub fn encode_xml_parameter_text(value: &str) -> String {
    let encoded = encode_xml_text(value);
    let chars: Vec<char> = encoded.chars().collect();
    let mut content_start = 0usize;
    while content_start < chars.len() && (chars[content_start] == '\r' || chars[content_start] == '\n') {
        content_start += 1;
    }
    let mut content_end = chars.len();
    while content_end > content_start && (chars[content_end - 1] == '\r' || chars[content_end - 1] == '\n') {
        content_end -= 1;
    }
    let prefix: String = chars[..content_start].iter().collect();
    let middle: String = chars[content_start..content_end].iter().collect();
    let suffix: String = chars[content_end..].iter().collect();
    format!("{}{}{}", encode_xml_line_breaks(&prefix), middle, encode_xml_line_breaks(&suffix))
}

pub fn encode_xml_attribute(value: &str) -> String {
    encode_xml_text(value).replace('"', "&quot;").replace('\'', "&apos;")
}

pub fn decode_xml_entities(value: &str) -> String {
    value
        .replace("&#13;", "\r")
        .replace("&#10;", "\n")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_text_entities() {
        assert_eq!(encode_xml_text("a<b>c&d"), "a&lt;b&gt;c&amp;d");
    }

    #[test]
    fn encode_xml_parameter_text_only_escapes_boundary_line_breaks() {
        assert_eq!(encode_xml_parameter_text("\n\rhello\nworld\r\n"), "&#10;&#13;hello\nworld&#13;&#10;");
        assert_eq!(encode_xml_parameter_text("plain"), "plain");
    }

    #[test]
    fn encodes_attribute_quotes() {
        assert_eq!(encode_xml_attribute("a\"b'c"), "a&quot;b&apos;c");
    }

    #[test]
    fn decode_reverses_all_entities() {
        assert_eq!(decode_xml_entities("a&lt;b&gt;c&amp;d&quot;e&apos;f&#13;&#10;"), "a<b>c&d\"e'f\r\n");
    }
}
