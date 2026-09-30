//! Port of senpi `packages/coding-agent/src/utils/text.ts` (the parts maho-core needs).

/// `stripBom`: removes a leading UTF-8 BOM.
pub fn strip_bom(text: &str) -> &str {
    text.strip_prefix('\u{feff}').unwrap_or(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_only_a_leading_bom() {
        assert_eq!(strip_bom("\u{feff}abc"), "abc");
        assert_eq!(strip_bom("abc"), "abc");
        assert_eq!(strip_bom("a\u{feff}b"), "a\u{feff}b");
    }
}
