//! Port of senpi packages/ai/src/utils/unavailable-tool-text.ts.

use regex::Regex;
use std::sync::LazyLock;

const MAX_LISTED_TOOLS: usize = 8;

fn escape_xml_attribute(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn available_tool_guidance(available_tool_names: &[String]) -> String {
    if available_tool_names.is_empty() {
        return "To edit files, call only tools available in this request.".into();
    }
    let listed = available_tool_names.iter().take(MAX_LISTED_TOOLS).cloned().collect::<Vec<_>>().join(", ");
    let omitted = available_tool_names.len().saturating_sub(MAX_LISTED_TOOLS);
    let suffix = if omitted > 0 { format!(" (and {omitted} more)") } else { String::new() };
    format!("To edit files, call your own tools: {listed}{suffix}.")
}

pub fn demoted_tool_call_text(name: &str, available_tool_names: &[String], first_occurrence: bool) -> String {
    let escaped = escape_xml_attribute(name);
    if !first_occurrence {
        return format!("<unavailable-tool-call name=\"{escaped}\"/>");
    }
    [
        format!("<unavailable-tool-call name=\"{escaped}\">"),
        "Transcript record, not an action available to you. An earlier model in this session".into(),
        format!("called \"{escaped}\"; that tool does not exist for you and its input is omitted."),
        available_tool_guidance(available_tool_names),
        "</unavailable-tool-call>".into(),
    ]
    .join("\n")
}

static CLOSING_TAG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new("(?i)</unavailable-tool-result").unwrap_or_else(|e| panic!("{e}")));

pub fn demoted_tool_result_text(name: &str, content: &str) -> String {
    let escaped = escape_xml_attribute(name);
    let safe = CLOSING_TAG.replace_all(content, |caps: &regex::Captures<'_>| format!("&lt;{}", &caps[0][1..]));
    format!("<unavailable-tool-result name=\"{escaped}\">{safe}</unavailable-tool-result>")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_call_and_result_text() {
        assert_eq!(demoted_tool_call_text("a\"b", &[], false), "<unavailable-tool-call name=\"a&quot;b\"/>");
        let tools: Vec<String> = (0..10).map(|i| format!("t{i}")).collect();
        let text = demoted_tool_call_text("x", &tools, true);
        assert!(text.ends_with("t0, t1, t2, t3, t4, t5, t6, t7 (and 2 more).\n</unavailable-tool-call>"));
        assert!(demoted_tool_call_text("x", &[], true).contains("call only tools available in this request."));
        assert_eq!(
            demoted_tool_result_text("n", "a</UNAVAILABLE-tool-result>"),
            "<unavailable-tool-result name=\"n\">a&lt;/UNAVAILABLE-tool-result></unavailable-tool-result>"
        );
    }
}
