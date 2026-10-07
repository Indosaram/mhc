//! Shared blocks of every Kibitzer envelope (latest `kibitzer/sidecar-prompt-blocks.ts`).
//!
//! Exact immutable behavior: `resolve_caps` merges PER-FIELD partial overrides over the defaults;
//! `single_line` collapses a whole `[\r\n]+` run to ONE space then trims; `field` caps in UTF-16
//! units including the `cap <= 1` branch.
//!
//! Honest N/A: JS `String` may hold a lone surrogate after a `.slice` that splits a pair; a Rust
//! `String` cannot, so `utf16_head` DROPS a trailing high surrogate. This differs from upstream only
//! for inputs whose cut lands mid-astral-character, which cannot be represented in Rust at all.

use memory_core::sync::redact::redact_url;

/// The model-visible tool registry of the resident sidecar, in registry order.
pub const KIBITZER_SIDECAR_TOOL_NAMES: [&str; 5] = ["read", "grep", "session_entries", "memory", "nudge"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KibitzerFieldCaps {
    pub prompt: usize,
    pub assistant: usize,
    pub tool_args: usize,
    pub result_head: usize,
    pub digest: usize,
    pub summary: usize,
    pub candidate: usize,
}

/// `Partial<KibitzerFieldCaps>`: every field optional so callers override a subset.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KibitzerFieldCapsOverride {
    pub prompt: Option<usize>,
    pub assistant: Option<usize>,
    pub tool_args: Option<usize>,
    pub result_head: Option<usize>,
    pub digest: Option<usize>,
    pub summary: Option<usize>,
    pub candidate: Option<usize>,
}

pub const KIBITZER_FIELD_CAPS: KibitzerFieldCaps = KibitzerFieldCaps {
    prompt: 4000, assistant: 1500, tool_args: 400, result_head: 600, digest: 1024, summary: 200, candidate: 200,
};

/// `resolveCaps(overrides)`: `{ ...KIBITZER_FIELD_CAPS, ...overrides }`, field by field.
pub fn resolve_caps(overrides: KibitzerFieldCapsOverride) -> KibitzerFieldCaps {
    KibitzerFieldCaps {
        prompt: overrides.prompt.unwrap_or(KIBITZER_FIELD_CAPS.prompt),
        assistant: overrides.assistant.unwrap_or(KIBITZER_FIELD_CAPS.assistant),
        tool_args: overrides.tool_args.unwrap_or(KIBITZER_FIELD_CAPS.tool_args),
        result_head: overrides.result_head.unwrap_or(KIBITZER_FIELD_CAPS.result_head),
        digest: overrides.digest.unwrap_or(KIBITZER_FIELD_CAPS.digest),
        summary: overrides.summary.unwrap_or(KIBITZER_FIELD_CAPS.summary),
        candidate: overrides.candidate.unwrap_or(KIBITZER_FIELD_CAPS.candidate),
    }
}

struct Rule { id: &'static str, text: &'static str }
const RULES: [Rule; 3] = [
    Rule { id: "no-memory-write", text: "You never write, edit, move or delete memory, files or state; the parent process owns every write." },
    Rule { id: "nudge-only", text: "Only the nudge tool reaches the primary agent; anything you write outside a tool call is discarded." },
    Rule { id: "silence-default", text: "Stay silent unless a stored memory would change what the primary agent does next." },
];

struct ToolContract { name: &'static str, args: &'static str, operations: Option<&'static str>, summary: &'static str }
const TOOL_CONTRACTS: [ToolContract; 5] = [
    ToolContract { name: "read", args: "path, offset?, limit?", operations: None, summary: "Read one workspace file; the result is capped." },
    ToolContract { name: "grep", args: "pattern, path?, glob?", operations: None, summary: "Search the workspace; the match list is capped." },
    ToolContract { name: "session_entries", args: "since", operations: None, summary: "Read the parent session entries after a cursor." },
    ToolContract { name: "memory", args: "operation, query|path", operations: Some("search,read"), summary: "Read memory: search and read only, there is no write operation." },
    ToolContract { name: "nudge", args: "path, hint", operations: None, summary: "Your only output: one candidate path and one factual hint of at most 200 characters." },
];

/// `renderContract()`.
pub fn render_contract() -> String {
    let mut lines = vec!["<contract>".to_string()];
    for rule in RULES {
        lines.push(format!("<rule id=\"{}\">{}</rule>", rule.id, escape_text(rule.text)));
    }
    for tool in TOOL_CONTRACTS {
        let attributes = match tool.operations {
            Some(operations) => vec![("name", tool.name), ("args", tool.args), ("operations", operations)],
            None => vec![("name", tool.name), ("args", tool.args)],
        };
        lines.push(format!("{}{}</tool>", open_tag("tool", &attributes), escape_text(tool.summary)));
    }
    lines.push("</contract>".to_string());
    lines.join("\n")
}

/// `renderTask(summary, caps)`.
pub fn render_task(summary: &str, caps: KibitzerFieldCaps) -> String {
    format!("<task>\n<summary>{}</summary>\n</task>", escape_text(&single_line(&field(summary, caps.summary))))
}

/// `openTag(name, attributes)`: attribute values are redacted, control-stripped and escaped.
pub fn open_tag(name: &str, attributes: &[(&str, &str)]) -> String {
    let rendered: String = attributes.iter().map(|(key, value)| format!(" {key}=\"{}\"", escape_attribute(value))).collect();
    format!("<{name}{rendered}>")
}

/// JS `String.length` in UTF-16 units.
fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// JS `String.slice(0, n)`, dropping a trailing high surrogate Rust cannot represent (see N/A note).
fn utf16_head(text: &str, n: usize) -> String {
    let mut units: Vec<u16> = text.encode_utf16().take(n).collect();
    if let Some(&last) = units.last()
        && (0xd800..=0xdbff).contains(&last)
    {
        units.pop();
    }
    String::from_utf16_lossy(&units)
}

/// One embedded field: redacted, then capped in UTF-16 units (`cap <= 1` included).
pub fn field(raw: &str, cap: usize) -> String {
    let redacted = strip_control(&redact_url(raw));
    if utf16_len(&redacted) <= cap {
        return redacted;
    }
    if cap <= 1 {
        return utf16_head(&redacted, cap);
    }
    format!("{}\u{2026}", utf16_head(&redacted, cap - 1))
}

/// `singleLine(value)`: `replace(/[\r\n]+/g, " ").trim()` - a whole run collapses to ONE space.
pub fn single_line(value: &str) -> String {
    let mut collapsed = String::with_capacity(value.len());
    let mut in_newline_run = false;
    for ch in value.chars() {
        if ch == '\r' || ch == '\n' {
            if !in_newline_run {
                collapsed.push(' ');
                in_newline_run = true;
            }
        } else {
            collapsed.push(ch);
            in_newline_run = false;
        }
    }
    collapsed.trim().to_string()
}

fn strip_control(value: &str) -> String {
    value.chars().filter(|ch| !matches!(ch, '\u{0000}'..='\u{0008}' | '\u{000b}' | '\u{000c}' | '\u{000e}'..='\u{001f}' | '\u{007f}')).collect()
}

/// `escapeText(value)`.
pub fn escape_text(value: &str) -> String {
    value.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('\'', "&apos;").replace('"', "&quot;")
}

fn escape_attribute(value: &str) -> String {
    escape_text(&strip_control(&redact_url(value))).replace('\n', "&#10;").replace('\r', "&#13;").replace('\t', "&#9;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn given_partial_overrides_when_resolved_then_only_named_fields_change() {
        let caps = resolve_caps(KibitzerFieldCapsOverride { summary: Some(50), ..Default::default() });
        assert_eq!(caps.summary, 50);
        assert_eq!(caps.prompt, KIBITZER_FIELD_CAPS.prompt);
        assert_eq!(caps.digest, KIBITZER_FIELD_CAPS.digest);
    }

    #[test]
    fn given_empty_overrides_when_resolved_then_defaults_are_returned() {
        assert_eq!(resolve_caps(KibitzerFieldCapsOverride::default()), KIBITZER_FIELD_CAPS);
    }

    #[test]
    fn given_a_newline_run_when_single_lined_then_it_collapses_to_one_space() {
        assert_eq!(single_line("a\n\n\r\nb"), "a b");
        assert_eq!(single_line("  \n  trimmed  "), "trimmed");
    }

    #[test]
    fn given_a_field_over_its_cap_when_capped_then_the_ellipsis_replaces_one_unit() {
        let capped = field("abcdef", 4);
        assert_eq!(utf16_len(&capped), 4);
        assert!(capped.ends_with('\u{2026}'));
    }

    #[test]
    fn given_cap_one_when_capped_then_a_single_unit_is_returned() {
        assert_eq!(utf16_len(&field("abcdef", 1)), 1);
        assert_eq!(field("abcdef", 0), "");
    }
}
