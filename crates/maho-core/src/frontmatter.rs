//! Port of senpi `packages/coding-agent/src/utils/frontmatter.ts`.
//!
//! Support module for the core resource ports (skills, prompt templates, themes). The senpi
//! source parses with the `yaml` npm package; this port parses with `serde_yaml`, so a
//! malformed document reports a serde_yaml message where senpi reports the `yaml` package's.

use serde_json::Value;

use crate::text::strip_bom;

/// `ParsedFrontmatter<T>`.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedFrontmatter {
    pub frontmatter: Value,
    pub body: String,
}

/// A parse failure carries the parser's message; senpi throws the same way from `parse()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrontmatterParseError {
    pub message: String,
}

impl std::fmt::Display for FrontmatterParseError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

fn normalize_newlines(value: &str) -> String {
    value.replace("\r\n", "\n").replace('\r', "\n")
}

/// `extractFrontmatter`: the YAML block between the opening and closing `---` fences, plus the body.
fn extract_frontmatter(content: &str) -> (Option<String>, String) {
    let normalized = normalize_newlines(strip_bom(content));

    if !normalized.starts_with("---") {
        return (None, normalized);
    }

    let Some(end_index) = normalized[3..].find("\n---").map(|offset| offset + 3) else {
        return (None, normalized);
    };

    let yaml_string = normalized.get(4..end_index).unwrap_or_default().to_string();
    let body = normalized.get(end_index + 4..).unwrap_or_default().trim().to_string();
    (Some(yaml_string), body)
}

/// `parseFrontmatter<T>`: parses the YAML block, defaulting to an empty object when absent.
pub fn parse_frontmatter(content: &str) -> Result<ParsedFrontmatter, FrontmatterParseError> {
    let (yaml_string, body) = extract_frontmatter(content);
    let Some(yaml_string) = yaml_string else {
        return Ok(ParsedFrontmatter { frontmatter: Value::Object(serde_json::Map::new()), body });
    };
    let parsed: Option<Value> = serde_yaml::from_str(&yaml_string).map_err(|error| FrontmatterParseError {
        message: error.to_string(),
    })?;
    Ok(ParsedFrontmatter {
        frontmatter: parsed.unwrap_or_else(|| Value::Object(serde_json::Map::new())),
        body,
    })
}

/// `stripFrontmatter`.
pub fn strip_frontmatter(content: &str) -> Result<String, FrontmatterParseError> {
    Ok(parse_frontmatter(content)?.body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_a_frontmatter_block_and_trims_the_body() {
        let parsed = parse_frontmatter("---\nname: demo\ndescription: hi\n---\n\nbody text\n").expect("parse");
        assert_eq!(parsed.frontmatter["name"], json!("demo"));
        assert_eq!(parsed.frontmatter["description"], json!("hi"));
        assert_eq!(parsed.body, "body text");
    }

    #[test]
    fn without_frontmatter_the_whole_document_is_the_body() {
        let parsed = parse_frontmatter("hello\nworld").expect("parse");
        assert_eq!(parsed.frontmatter, json!({}));
        assert_eq!(parsed.body, "hello\nworld");
    }

    #[test]
    fn normalizes_crlf_and_strips_a_bom() {
        let parsed = parse_frontmatter("\u{feff}---\r\nname: demo\r\n---\r\nbody\r\n").expect("parse");
        assert_eq!(parsed.frontmatter["name"], json!("demo"));
        assert_eq!(parsed.body, "body");
    }

    #[test]
    fn an_unclosed_fence_is_not_frontmatter() {
        let parsed = parse_frontmatter("---\nname: demo\n").expect("parse");
        assert_eq!(parsed.frontmatter, json!({}));
        assert_eq!(parsed.body, "---\nname: demo\n");
    }

    #[test]
    fn an_invalid_document_reports_a_parse_error() {
        let error = parse_frontmatter("---\nname: [unclosed\n---\nbody").expect_err("parse error");
        assert!(!error.message.is_empty());
    }
}
