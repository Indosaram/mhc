//! Shortening oversized memory descriptions before the reflection completion gate runs.
//!
//! Behaviour spec: `docs/cleanroom/specs/memory-description-preflight.md`. The gate rejects a
//! description over 1024 characters, and a reflection whose only defect is length would otherwise be
//! thrown away along with an hour of work. This pass repairs exactly that defect and nothing else: it
//! touches only the `description:` value, only when the value is too long, and only after the branch is
//! confirmed clean.
use crate::frontmatter::MAX_DESCRIPTION_LENGTH;

/// One description this pass shortened, with the lengths the commit trailer records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrimmedDescription {
    pub path: String,
    pub original_length: usize,
    pub new_length: usize,
}

/// The measure the validator reports, so the preflight and the gate agree on what "oversized" means.
///
/// This counts characters, matching the `'description' exceeds 1024 characters (<n>)` message. It is
/// deliberately not the same measure the trimmer budgets in; see [`trim_description`].
pub fn description_length(value: &str) -> usize {
    value.chars().count()
}

pub fn is_oversized(description: &str) -> bool {
    description_length(description) > MAX_DESCRIPTION_LENGTH
}

/// Cut to one unit under the cap, prefer a word boundary, and end with an ellipsis.
///
/// The budget is counted in **UTF-16 units**, not characters, because that is the measure the original
/// contract used: a JS string is a sequence of UTF-16 units, and a 1024-unit cap was always a cap on
/// that. The two measures agree for ASCII and for every BMP script including Korean, and diverge only on
/// astral characters such as emoji, where one character is two units. Budgeting in units therefore keeps
/// the result under the reported character cap too — every character is at least one unit, so a prefix
/// of at most 1023 units is at most 1023 characters, plus the ellipsis.
///
/// Splitting on a code point boundary is not optional: slicing a UTF-8 string at a byte offset inside a
/// character panics.
pub fn trim_description(original: &str) -> String {
    let mut prefix = String::new();
    let mut units = 0usize;
    for code_point in original.chars() {
        if units + code_point.len_utf16() > MAX_DESCRIPTION_LENGTH - 1 {
            break;
        }
        prefix.push(code_point);
        units += code_point.len_utf16();
    }
    // Prefer cutting at a space: a description ending mid-word reads as corruption rather than as a
    // shortened sentence. A cut that already lands just before a space needs no search.
    let boundary = if original[prefix.len()..].starts_with(' ') {
        Some(prefix.len())
    } else {
        prefix.rfind(' ')
    };
    match boundary {
        Some(index) if index > 0 => format!("{}…", original[..index].trim_end()),
        _ => format!("{}…", prefix.trim_end()),
    }
}

/// Replace the value of the first `description:` line, leaving every other byte untouched.
///
/// Other keys, key order, and CRLF endings survive because the alternative — re-rendering the whole
/// header — would also normalize values the gate did not complain about, turning a length repair into an
/// unrelated rewrite.
pub fn replace_description_line(header: &str, rendered: &str) -> String {
    let mut output = String::with_capacity(header.len());
    let mut replaced = false;
    let mut rest = header;
    while !rest.is_empty() {
        let (line, next) = match rest.find('\n') {
            Some(index) => (&rest[..=index], &rest[index + 1..]),
            None => (rest, ""),
        };
        let (core, terminator) = match line.strip_suffix('\n') {
            Some(core) => match core.strip_suffix('\r') {
                Some(core) => (core, "\r\n"),
                None => (core, "\n"),
            },
            None => (line, ""),
        };
        match description_spacing_end(core) {
            Some(spacing_end) if !replaced => {
                output.push_str(&core[..spacing_end]);
                output.push_str(rendered);
                output.push_str(terminator);
                replaced = true;
            }
            _ => output.push_str(line),
        }
        rest = next;
    }
    output
}

/// Byte offset just past the `description:` key and its mandatory spacing, when `line` is that key.
///
/// The spacing requirement is what keeps `description:value` (not a header line under the strict
/// grammar) from being rewritten as though it were one.
pub fn description_spacing_end(line: &str) -> Option<usize> {
    let rest = line.strip_prefix("description:")?;
    let spacing = rest.len() - rest.trim_start_matches([' ', '\t']).len();
    (spacing > 0).then(|| "description:".len() + spacing)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn long(words: usize) -> String {
        (0..words)
            .map(|index| format!("word{index}"))
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn the_cap_is_counted_in_characters() {
        let exact = "a".repeat(MAX_DESCRIPTION_LENGTH);
        assert!(!is_oversized(&exact));
        assert!(is_oversized(&"a".repeat(MAX_DESCRIPTION_LENGTH + 1)));
    }

    #[test]
    fn a_korean_description_is_counted_per_character() {
        assert!(!is_oversized(&"가".repeat(MAX_DESCRIPTION_LENGTH)));
        assert!(is_oversized(&"가".repeat(MAX_DESCRIPTION_LENGTH + 1)));
    }

    #[test]
    fn an_oversized_ascii_description_is_shortened_under_the_cap() {
        let trimmed = trim_description(&long(400));
        assert!(description_length(&trimmed) <= MAX_DESCRIPTION_LENGTH);
        assert!(trimmed.ends_with('…'));
    }

    #[test]
    fn shortening_prefers_a_word_boundary() {
        let trimmed = trim_description(&long(400));
        let body = trimmed.trim_end_matches('…');
        assert!(!body.ends_with("wor"), "cut mid-word: {body}");
        assert!(body.ends_with(|ch: char| ch.is_ascii_digit()), "{body}");
    }

    #[test]
    fn a_single_unbroken_token_is_cut_without_a_boundary() {
        let trimmed = trim_description(&"x".repeat(2000));
        assert!(description_length(&trimmed) <= MAX_DESCRIPTION_LENGTH);
        assert!(trimmed.ends_with('…'));
        assert!(!trimmed.trim_end_matches('…').contains(' '));
    }

    #[test]
    fn an_oversized_korean_description_shortens_on_a_character_boundary() {
        let trimmed = trim_description(&"가".repeat(2000));
        assert!(description_length(&trimmed) <= MAX_DESCRIPTION_LENGTH);
        assert!(trimmed.ends_with('…'));
    }

    #[test]
    fn an_astral_character_is_never_split() {
        let trimmed = trim_description(&"😀".repeat(2000));
        assert!(description_length(&trimmed) <= MAX_DESCRIPTION_LENGTH);
        assert!(trimmed.chars().all(|ch| ch == '😀' || ch == '…'));
        assert!(trimmed.ends_with('…'));
    }

    #[test]
    fn an_astral_tail_is_budgeted_in_units_so_the_character_cap_holds() {
        let mut text = "😀".repeat(511);
        text.push_str(" tail words here");
        let trimmed = trim_description(&text);
        assert!(description_length(&trimmed) <= MAX_DESCRIPTION_LENGTH);
    }

    #[test]
    fn shortening_trims_trailing_spaces_before_the_ellipsis() {
        let trimmed = trim_description(&format!("{}   {}", "a".repeat(1020), "b".repeat(50)));
        assert!(!trimmed.contains(" …"));
    }

    #[test]
    fn a_description_with_a_space_exactly_at_the_cut_needs_no_search() {
        let text = format!("{} next", "a".repeat(MAX_DESCRIPTION_LENGTH - 1));
        let trimmed = trim_description(&text);
        assert_eq!(
            trimmed,
            format!("{}…", "a".repeat(MAX_DESCRIPTION_LENGTH - 1))
        );
    }

    #[test]
    fn the_first_description_line_is_the_one_replaced() {
        let header = "description: old\nkind: note\ndescription: second\n";
        assert_eq!(
            replace_description_line(header, "new"),
            "description: new\nkind: note\ndescription: second\n"
        );
    }

    #[test]
    fn a_crlf_header_keeps_its_line_endings() {
        let header = "description: old\r\nkind: note\r\n";
        assert_eq!(
            replace_description_line(header, "new"),
            "description: new\r\nkind: note\r\n"
        );
    }

    #[test]
    fn other_keys_and_their_order_are_untouched() {
        let header = "zeta: 1\ndescription: old\nalpha: 2\n";
        assert_eq!(
            replace_description_line(header, "new"),
            "zeta: 1\ndescription: new\nalpha: 2\n"
        );
    }

    #[test]
    fn tabs_after_the_key_are_preserved_as_spacing() {
        let header = "description:\told\n";
        assert_eq!(
            replace_description_line(header, "new"),
            "description:\tnew\n"
        );
    }

    #[test]
    fn a_key_without_spacing_is_not_a_description_line() {
        assert_eq!(description_spacing_end("description:value"), None);
        assert_eq!(description_spacing_end("descriptions: value"), None);
        assert_eq!(description_spacing_end("kind: note"), None);
        assert_eq!(description_spacing_end("description: "), Some(13));
    }

    #[test]
    fn a_header_without_a_description_line_is_returned_unchanged() {
        let header = "kind: note\n";
        assert_eq!(replace_description_line(header, "new"), header);
    }

    #[test]
    fn a_header_with_no_trailing_newline_still_replaces() {
        assert_eq!(
            replace_description_line("description: old", "new"),
            "description: new"
        );
    }

    #[test]
    fn a_shortened_description_replaces_a_quoted_one_cleanly() {
        let header = "description: \"a: b\"\n";
        let rendered = "a shortened value";
        assert_eq!(
            replace_description_line(header, rendered),
            "description: a shortened value\n"
        );
    }
}
