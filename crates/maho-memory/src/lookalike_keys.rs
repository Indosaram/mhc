//! When a frontmatter key is spelled to look like a protected one.
//!
//! Spec: `docs/cleanroom/specs/memory-lookalike-keys.md`. `рrovenance` with a Cyrillic `р` is not the
//! protected key, so a writer that only compares bytes happily adds a second, invisible field. The person
//! reading the file sees one field; the agent sees an unprotected extra one.

use unicode_general_category::{GeneralCategory, get_general_category};
use unicode_normalization::UnicodeNormalization;

pub const PROTECTED_KEYS: &[&str] = &[
    "description",
    "read_only",
    "kind",
    "aliases",
    "limit",
    "provenance",
    "expires",
];

fn confusable_char(ch: char) -> Option<char> {
    match ch {
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

/// Invisible characters: combining marks, format characters, and the zero-width fillers that render as
/// nothing but split a key into a different byte sequence.
fn is_invisible(ch: char) -> bool {
    let category = get_general_category(ch);
    category == GeneralCategory::NonspacingMark
        || category == GeneralCategory::Format
        || matches!(
            ch as u32,
            0x115F | 0x1160 | 0x180E | 0x3164 | 0xFFA0 | 0xFFF0..=0xFFF8 | 0xE0000..=0xE0FFF
        )
}

/// How a reader might take a word that is written to look like another: compatibility forms folded,
/// invisible and combining marks removed, common Cyrillic and Greek letters mapped to the Latin letter they
/// resemble, then case folded.
pub fn look_alike_skeleton(text: &str) -> String {
    let folded: String = text.nfkc().collect();
    let visible: String = folded.chars().filter(|ch| !is_invisible(*ch)).collect();
    visible
        .chars()
        .map(|ch| confusable_char(ch).unwrap_or(ch))
        .collect::<String>()
        .to_lowercase()
}

pub fn normalized_key(key: &str) -> String {
    look_alike_skeleton(key)
        .trim_matches(|ch| ch == '"' || ch == '\'')
        .trim()
        .to_owned()
}

pub fn look_alike_keys<'a, I>(keys: I, canonical: &str) -> Vec<String>
where
    I: IntoIterator<Item = &'a str>,
{
    keys.into_iter()
        .filter(|key| *key != canonical && normalized_key(key) == canonical)
        .map(str::to_owned)
        .collect()
}

pub fn impersonated_protected_key(key: &str) -> Option<&'static str> {
    let skeleton = normalized_key(key);
    PROTECTED_KEYS
        .iter()
        .copied()
        .find(|canonical| *canonical == skeleton && *canonical != key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_lowercase_keys_fold_to_themselves() {
        assert_eq!(look_alike_skeleton("provenance"), "provenance");
        assert_eq!(look_alike_skeleton("Description"), "description");
    }

    #[test]
    fn a_zero_width_space_is_removed() {
        assert_eq!(look_alike_skeleton("provenance\u{200b}"), "provenance");
    }

    #[test]
    fn a_combining_grapheme_joiner_is_removed() {
        assert_eq!(look_alike_skeleton("provenance\u{034f}"), "provenance");
    }

    #[test]
    fn a_cyrillic_er_reads_as_p() {
        assert_eq!(look_alike_skeleton("\u{0440}rovenance"), "provenance");
    }

    #[test]
    fn cyrillic_and_greek_letters_map_to_latin() {
        assert_eq!(
            look_alike_skeleton("\u{0430}\u{0435}\u{043e}\u{0441}\u{0443}\u{0445}"),
            "aeocyx"
        );
        assert_eq!(
            look_alike_skeleton("\u{03b1}\u{03b5}\u{03bf}\u{03c1}"),
            "aeop"
        );
    }

    #[test]
    fn uppercase_confusables_fold_too() {
        assert_eq!(
            look_alike_skeleton("\u{0420}\u{041e}\u{0412}\u{0415}\u{041d}"),
            "pobeh"
        );
    }

    #[test]
    fn compatibility_forms_fold_before_mapping() {
        assert_eq!(look_alike_skeleton("\u{ff50}rovenance"), "provenance");
    }

    #[test]
    fn surrounding_quotes_and_space_are_trimmed() {
        assert_eq!(normalized_key("\"provenance\""), "provenance");
        assert_eq!(normalized_key("'read_only'"), "read_only");
        assert_eq!(normalized_key("  kind  "), "kind");
    }

    #[test]
    fn a_byte_identical_key_is_not_an_impostor() {
        assert!(look_alike_keys(["description"], "description").is_empty());
    }

    #[test]
    fn a_look_alike_key_is_reported() {
        let found = look_alike_keys(["description", "\u{0440}rovenance"], "provenance");
        assert_eq!(found, vec!["\u{0440}rovenance".to_owned()]);
    }

    #[test]
    fn an_unrelated_key_is_not_reported() {
        assert!(look_alike_keys(["kind", "expires"], "provenance").is_empty());
    }

    #[test]
    fn every_look_alike_of_a_protected_key_is_caught() {
        for canonical in PROTECTED_KEYS {
            let impostor = format!("\u{0440}{}", &canonical[1..]);
            if canonical.starts_with('p') {
                assert_eq!(impersonated_protected_key(&impostor), Some(*canonical));
            }
        }
    }

    #[test]
    fn a_protected_key_spelled_exactly_is_not_an_impostor() {
        for canonical in PROTECTED_KEYS {
            assert_eq!(impersonated_protected_key(canonical), None, "{canonical}");
        }
    }

    #[test]
    fn an_ordinary_extra_key_is_not_an_impostor() {
        assert_eq!(impersonated_protected_key("created_at"), None);
        assert_eq!(impersonated_protected_key("my-note"), None);
    }

    #[test]
    fn a_quoted_impostor_is_still_caught() {
        assert_eq!(
            impersonated_protected_key("\"\u{0440}rovenance\""),
            Some("provenance")
        );
    }
}
