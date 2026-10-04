use crate::types::{RuleSource, TtsrInterruptMode, TtsrRule, TtsrScope};
pub const FABRICATED_UNAVAILABLE_TOOL_CALL_RULE_NAME: &str = "fabricated-unavailable-tool-call";
pub fn builtin_ttsr_rules() -> Vec<TtsrRule> {
    vec![TtsrRule { name: FABRICATED_UNAVAILABLE_TOOL_CALL_RULE_NAME.into(), path: None,
        content: ["Your previous output imitated an unavailable historical tool call as inert text instead of taking action.", "Redo the interrupted step now. Call your real tools that are actually available to you, such as edit or write for file changes.", "Do not print or imitate unavailable-tool transcript envelopes."].join("\n"),
        description: Some("Interrupt fabricated unavailable-tool calls emitted as assistant text.".into()), globs: None,
        condition: vec![r"(?i)<\s*unavailable-tool-call\b".into(), r#"(?i)\[called\s+tool\s+["'][^"'\r\n]+["']\s+\(no\s+longer\s+available\s+in\s+this\s+session\)"#.into()],
        scope: TtsrScope { allow_text: true, allow_thinking: false, tool_scopes: vec![] }, interrupt_mode: TtsrInterruptMode::Always, source: RuleSource::Builtin }]
}
