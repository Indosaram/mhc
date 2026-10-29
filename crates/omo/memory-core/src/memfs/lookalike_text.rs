//! Text skeleton normalization for confusable look-alike detection.

use unicode_general_category::{GeneralCategory, get_general_category};
use unicode_normalization::UnicodeNormalization;

fn confusable_char(c: char) -> Option<char> {
    match c {
        '\u{0430}' => Some('a'),
        '\u{0435}' => Some('e'),
        '\u{043e}' => Some('o'),
        '\u{0440}' => Some('p'),
        '\u{0441}' => Some('c'),
        '\u{0443}' => Some('y'),
        '\u{0445}' => Some('x'),
        '\u{0456}' => Some('i'),
        '\u{0458}' => Some('j'),
        '\u{0455}' => Some('s'),
        '\u{0501}' => Some('d'),
        '\u{051b}' => Some('q'),
        '\u{051d}' => Some('w'),
        '\u{04cf}' => Some('l'),
        '\u{0410}' => Some('a'),
        '\u{0415}' => Some('e'),
        '\u{041e}' => Some('o'),
        '\u{0420}' => Some('p'),
        '\u{0421}' => Some('c'),
        '\u{0425}' => Some('x'),
        '\u{0406}' => Some('i'),
        '\u{0408}' => Some('j'),
        '\u{0405}' => Some('s'),
        '\u{041a}' => Some('k'),
        '\u{041c}' => Some('m'),
        '\u{041d}' => Some('h'),
        '\u{0422}' => Some('t'),
        '\u{0412}' => Some('b'),
        '\u{03b1}' => Some('a'),
        '\u{03b5}' => Some('e'),
        '\u{03bf}' => Some('o'),
        '\u{03c1}' => Some('p'),
        '\u{03c4}' => Some('t'),
        '\u{03c5}' => Some('u'),
        '\u{03bd}' => Some('v'),
        '\u{03ba}' => Some('k'),
        '\u{03b9}' => Some('i'),
        '\u{039f}' => Some('o'),
        '\u{0391}' => Some('a'),
        '\u{0392}' => Some('b'),
        '\u{0395}' => Some('e'),
        '\u{0397}' => Some('h'),
        '\u{0399}' => Some('i'),
        '\u{039a}' => Some('k'),
        '\u{039c}' => Some('m'),
        '\u{039d}' => Some('n'),
        '\u{03a1}' => Some('p'),
        '\u{03a4}' => Some('t'),
        '\u{03a5}' => Some('y'),
        '\u{03a7}' => Some('x'),
        '\u{0131}' => Some('i'),
        '\u{0130}' => Some('i'),
        _ => None,
    }
}

fn is_invisible(c: char) -> bool {
    let cat = get_general_category(c);
    cat == GeneralCategory::NonspacingMark
        || cat == GeneralCategory::Format
        || matches!(
            c as u32,
            0x115F | 0x1160 | 0x180E | 0x3164 | 0xFFA0 | 0xFFF0..=0xFFF8 | 0xE0000..=0xE0FFF
        )
}

/// How a reader might take a word that is written to look like another:
/// compatibility forms folded (NFKC), invisible and combining marks removed,
/// common Cyrillic and Greek letters that look Latin mapped to it, and case folded.
pub fn look_alike_skeleton(text: &str) -> String {
    let nfkc: String = text.nfkc().collect();
    let without_invisible: String = nfkc.chars().filter(|&c| !is_invisible(c)).collect();
    let mapped: String = without_invisible
        .chars()
        .map(|c| confusable_char(c).unwrap_or(c))
        .collect();
    mapped.to_lowercase()
}
