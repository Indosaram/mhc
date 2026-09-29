//! Port of senpi packages/ai/src/utils/tool-call-id.ts.

use super::hash::short_hash;

pub const TOOL_CALL_ID_MAX_LENGTH: usize = 64;

/// Normalize tool call ids to the Anthropic-family pattern (max 64 chars, alphanumeric/underscore/dash);
/// over-long ids keep a readable prefix plus a hash of the full id so they do not collide.
pub fn normalize_tool_call_id(id: &str) -> String {
    let sanitized: String = id
        .encode_utf16()
        .map(|unit| match char::from_u32(u32::from(unit)) {
            Some(c) if c.is_ascii_alphanumeric() || c == '_' || c == '-' => c,
            _ => '_',
        })
        .collect();
    if sanitized.len() <= TOOL_CALL_ID_MAX_LENGTH {
        return sanitized;
    }
    let suffix = format!("_{}", short_hash(id));
    format!("{}{}", &sanitized[..TOOL_CALL_ID_MAX_LENGTH - suffix.len()], suffix)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_and_truncates_with_hash() {
        assert_eq!(normalize_tool_call_id("call|abc.1"), "call_abc_1");
        let long = "x".repeat(100);
        let normalized = normalize_tool_call_id(&long);
        assert_eq!(normalized.len(), TOOL_CALL_ID_MAX_LENGTH);
        assert!(normalized.ends_with(&format!("_{}", short_hash(&long))));
        assert_eq!(normalize_tool_call_id("🙈"), "__");
    }
}
