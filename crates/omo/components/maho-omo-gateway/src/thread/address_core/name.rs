//! Thread & device name normalization (`packages/thread-address-core/src/name.ts`).

use unicode_normalization::UnicodeNormalization;

/// The one name comparison for threads and devices: NFKC, trim, lowercase, whitespace collapsed,
/// so a composed and a decomposed spelling of the same name are one name.
pub fn normalize_name(value: &str) -> String {
    let nfkc: String = value.nfkc().collect();
    let mut out = String::with_capacity(nfkc.len());
    let mut first = true;
    for word in nfkc.split_whitespace() {
        if !first {
            out.push(' ');
        }
        first = false;
        for ch in word.chars() {
            for lc in ch.to_lowercase() {
                out.push(lc);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_nfkc_case_and_whitespace() {
        assert_eq!(normalize_name("  Hello\t\n  WORLD "), "hello world");
        assert_eq!(normalize_name("\u{0041}\u{030a}"), "\u{00e5}");
    }
}
