//! Parsing `git status --porcelain` output.
//!
//! Behaviour spec: `docs/cleanroom/specs/memory-git-status.md`. The memory store decides which
//! Markdown files changed from this output, so a path this parser fails to decode is a file the
//! store silently stops tracking.
use std::fmt;

/// What happened to one path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    Added,
    Modified,
    Deleted,
    /// The original path is in [`StatusEntry::from`].
    Renamed,
    Copied,
    Untracked,
    Unmerged,
    /// A code this build does not model, carried through unchanged.
    Other,
}

impl Change {
    fn from_code(code: char) -> Self {
        match code {
            'A' => Self::Added,
            'M' => Self::Modified,
            'D' => Self::Deleted,
            'R' => Self::Renamed,
            'C' => Self::Copied,
            '?' => Self::Untracked,
            'U' => Self::Unmerged,
            _ => Self::Other,
        }
    }

    /// Whether the path still holds content in the working tree.
    ///
    /// A deleted memory must not be re-read: reading it would either fail or, worse, read a
    /// same-named file that is no longer the memory this entry describes.
    pub fn still_present(self) -> bool {
        !matches!(self, Self::Deleted)
    }
}

/// One path from the status output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusEntry {
    pub index: Change,
    pub worktree: Change,
    pub path: String,
    pub from: Option<String>,
}

impl StatusEntry {
    /// Whether either side reports a deletion.
    pub fn is_deleted(&self) -> bool {
        self.index == Change::Deleted || self.worktree == Change::Deleted
    }

    /// Whether the entry describes content the store should read.
    pub fn is_readable_content(&self) -> bool {
        !self.is_deleted()
    }
}

/// Why a status line could not be parsed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StatusError {
    /// The line is shorter than the two status columns and the separating space.
    TooShort {
        line: usize,
    },
    UnterminatedQuote {
        line: usize,
    },
    BadEscape {
        line: usize,
        /// The offending sequence, e.g. `\q`.
        sequence: String,
    },
    BadOctal {
        line: usize,
    },
}

impl fmt::Display for StatusError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooShort { line } => write!(f, "line {line}: shorter than a status entry"),
            Self::UnterminatedQuote { line } => write!(f, "line {line}: unterminated quoted path"),
            Self::BadEscape { line, sequence } => {
                write!(f, "line {line}: unsupported escape {sequence}")
            }
            Self::BadOctal { line } => write!(f, "line {line}: invalid octal escape"),
        }
    }
}

impl std::error::Error for StatusError {}

/// Decode one quoted path body.
///
/// Git quotes a path when it contains anything outside printable ASCII, and escapes the bytes the
/// way C does: `\t`, `\n`, `\r`, `\\`, `\"` and `\NNN` octal. The octal escapes are **bytes**, not
/// characters, so they are collected and only then decoded as UTF-8 — a Korean file name arrives as
/// three octal escapes per character and must come back as one character.
fn unquote(body: &str, line: usize) -> Result<String, StatusError> {
    let mut bytes: Vec<u8> = Vec::with_capacity(body.len());
    let mut chars = body.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            let mut buffer = [0u8; 4];
            bytes.extend_from_slice(ch.encode_utf8(&mut buffer).as_bytes());
            continue;
        }
        let Some(escape) = chars.next() else {
            return Err(StatusError::BadEscape {
                line,
                sequence: "\\".to_owned(),
            });
        };
        match escape {
            't' => bytes.push(b'\t'),
            'n' => bytes.push(b'\n'),
            'r' => bytes.push(b'\r'),
            'a' => bytes.push(0x07),
            'b' => bytes.push(0x08),
            'f' => bytes.push(0x0c),
            'v' => bytes.push(0x0b),
            '\\' => bytes.push(b'\\'),
            '"' => bytes.push(b'"'),
            digit @ '0'..='7' => {
                let mut value = digit.to_digit(8).expect("octal digit") as u8;
                for _ in 0..2 {
                    let next = chars.next().ok_or(StatusError::BadOctal { line })?;
                    let digit = next.to_digit(8).ok_or(StatusError::BadOctal { line })?;
                    value = value
                        .checked_mul(8)
                        .and_then(|value| value.checked_add(digit as u8))
                        .ok_or(StatusError::BadOctal { line })?;
                }
                bytes.push(value);
            }
            other => {
                return Err(StatusError::BadEscape {
                    line,
                    sequence: format!("\\{other}"),
                });
            }
        }
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn decode_path(raw: &str, line: usize) -> Result<String, StatusError> {
    if let Some(body) = raw.strip_prefix('"') {
        let body = body
            .strip_suffix('"')
            .ok_or(StatusError::UnterminatedQuote { line })?;
        return unquote(body, line);
    }
    Ok(raw.to_owned())
}

/// Split everything after the status columns into `(from, to)`.
///
/// Git writes a rename or copy as `from -> to`, so the destination is the second path. A quoted path
/// may itself contain ` -> `, so the separator is only honoured outside quotes.
fn split_rename(raw: &str) -> (&str, Option<&str>) {
    let bytes = raw.as_bytes();
    let mut quoted = false;
    let mut escaped = false;
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if escaped {
            escaped = false;
        } else if byte == b'\\' && quoted {
            escaped = true;
        } else if byte == b'"' {
            quoted = !quoted;
        } else if !quoted && raw[index..].starts_with(" -> ") {
            return (&raw[..index], Some(&raw[index + 4..]));
        }
        index += 1;
    }
    (raw, None)
}

/// Parse one status line.
///
/// The untouched form is `XY <path>`, where column 1 is the index and column 2 the working tree.
/// A rename or copy carries both paths separated by ` -> `.
pub fn parse_line(line: &str, line_number: usize) -> Result<Option<StatusEntry>, StatusError> {
    let line = line.trim_end_matches('\r');
    if line.trim().is_empty() {
        return Ok(None);
    }
    if line.len() < 4 || !line.is_char_boundary(2) || !line.is_char_boundary(3) {
        return Err(StatusError::TooShort { line: line_number });
    }
    let mut codes = line[..2].chars();
    let index_code = codes.next().expect("two status columns");
    let worktree_code = codes.next().expect("two status columns");
    if line.as_bytes()[2] != b' ' {
        return Err(StatusError::TooShort { line: line_number });
    }
    let index = Change::from_code(index_code);
    let worktree = Change::from_code(worktree_code);
    let (first, second) = split_rename(&line[3..]);
    let (path_raw, from_raw) = match second {
        Some(to) => (to, Some(first)),
        None => (first, None),
    };
    let path = decode_path(path_raw, line_number)?;
    let from = match from_raw {
        Some(raw) => Some(decode_path(raw, line_number)?),
        None => None,
    };
    Ok(Some(StatusEntry {
        index,
        worktree,
        path,
        from,
    }))
}

/// Parse a whole status output.
///
/// `git status --porcelain` never emits a header, so every line is data; an unparsable line fails
/// the whole parse rather than being skipped, because skipping would silently stop tracking the
/// path it named.
pub fn parse(source: &str) -> Result<Vec<StatusEntry>, StatusError> {
    let mut entries = Vec::new();
    for (index, line) in source.lines().enumerate() {
        if let Some(entry) = parse_line(line, index + 1)? {
            entries.push(entry);
        }
    }
    Ok(entries)
}

/// The paths that changed and still hold content, in output order.
///
/// A rename contributes only its destination: the original no longer exists, and reading it would
/// resurrect content the user moved away.
pub fn changed_paths(source: &str) -> Result<Vec<String>, StatusError> {
    Ok(parse(source)?
        .into_iter()
        .filter(|entry| entry.is_readable_content())
        .map(|entry| entry.path)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(line: &str) -> StatusEntry {
        parse_line(line, 1).expect("parse").expect("entry")
    }

    #[test]
    fn an_unmodified_path_is_added() {
        let parsed = entry("A  notes/new.md");
        assert_eq!(parsed.index, Change::Added);
        assert_eq!(parsed.worktree, Change::Other);
        assert_eq!(parsed.path, "notes/new.md");
    }

    #[test]
    fn a_working_tree_modification_is_read() {
        let parsed = entry(" M notes/changed.md");
        assert_eq!(parsed.index, Change::Other);
        assert_eq!(parsed.worktree, Change::Modified);
        assert_eq!(parsed.path, "notes/changed.md");
        assert!(parsed.is_readable_content());
    }

    #[test]
    fn a_deleted_path_is_not_readable_content() {
        assert!(!entry(" D notes/gone.md").is_readable_content());
        assert!(!entry("D  notes/gone.md").is_readable_content());
        assert!(entry(" D notes/gone.md").is_deleted());
    }

    #[test]
    fn an_untracked_path_is_read() {
        let parsed = entry("?? notes/draft.md");
        assert_eq!(parsed.index, Change::Untracked);
        assert_eq!(parsed.path, "notes/draft.md");
    }

    #[test]
    fn a_rename_reports_both_paths() {
        let parsed = entry("R  old.md -> new.md");
        assert_eq!(parsed.index, Change::Renamed);
        assert_eq!(parsed.path, "new.md");
        assert_eq!(parsed.from.as_deref(), Some("old.md"));
    }

    #[test]
    fn a_copy_reports_both_paths() {
        let parsed = entry("C  source.md -> copy.md");
        assert_eq!(parsed.index, Change::Copied);
        assert_eq!(parsed.path, "copy.md");
        assert_eq!(parsed.from.as_deref(), Some("source.md"));
    }

    #[test]
    fn an_octal_quoted_korean_path_is_decoded_to_characters() {
        let parsed = entry("?? \"notes/\\355\\225\\234\\352\\270\\200.md\"");
        assert_eq!(parsed.path, "notes/한글.md");
    }

    #[test]
    fn a_quoted_path_without_escapes_is_decoded() {
        assert_eq!(
            entry("?? \"notes/plain path.md\"").path,
            "notes/plain path.md"
        );
    }

    #[test]
    fn a_rename_with_quoted_paths_reports_both_decoded() {
        let parsed = entry("R  \"\\355\\225\\234.md\" -> \"\\352\\270\\200.md\"");
        assert_eq!(parsed.path, "글.md");
        assert_eq!(parsed.from.as_deref(), Some("한.md"));
    }

    #[test]
    fn a_rename_separator_inside_a_quoted_path_is_not_a_rename() {
        let parsed = entry("?? \"notes/a -> b.md\"");
        assert_eq!(parsed.path, "notes/a -> b.md");
        assert_eq!(parsed.from, None);
    }

    #[test]
    fn tab_and_newline_escapes_are_decoded() {
        assert_eq!(entry("?? \"a\\tb\"").path, "a\tb");
        assert_eq!(entry("?? \"a\\nb\"").path, "a\nb");
    }

    #[test]
    fn a_backslash_escape_is_decoded_to_one_backslash() {
        assert_eq!(entry("?? \"a\\\\b\"").path, "a\\b");
    }

    #[test]
    fn an_unterminated_quote_is_an_error() {
        assert_eq!(
            parse_line("?? \"notes/half.md", 4),
            Err(StatusError::UnterminatedQuote { line: 4 })
        );
    }

    #[test]
    fn an_unknown_escape_is_an_error() {
        assert!(matches!(
            parse_line("?? \"a\\qb\"", 1),
            Err(StatusError::BadEscape { .. })
        ));
    }

    #[test]
    fn an_incomplete_octal_escape_is_an_error() {
        assert!(matches!(
            parse_line("?? \"a\\35\"", 1),
            Err(StatusError::BadOctal { .. })
        ));
    }

    #[test]
    fn a_line_shorter_than_the_status_columns_is_an_error() {
        assert_eq!(parse_line("ab", 2), Err(StatusError::TooShort { line: 2 }));
    }

    #[test]
    fn a_blank_line_yields_no_entry() {
        assert_eq!(parse_line("", 1), Ok(None));
        assert_eq!(parse_line("   ", 1), Ok(None));
    }

    #[test]
    fn a_trailing_carriage_return_is_ignored() {
        assert_eq!(entry("A  notes/new.md\r").path, "notes/new.md");
    }

    #[test]
    fn changed_paths_skips_deletions_and_keeps_everything_else() {
        let output = "A  a.md\n D b.md\n?? c.md\nR  old.md -> new.md\n";
        assert_eq!(
            changed_paths(output).expect("parse"),
            ["a.md", "c.md", "new.md"]
        );
    }

    #[test]
    fn a_bad_line_fails_the_whole_parse_instead_of_being_skipped() {
        let output = "A  a.md\n?? \"unterminated\n";
        assert!(matches!(
            parse(output),
            Err(StatusError::UnterminatedQuote { line: 2 })
        ));
    }

    #[test]
    fn an_error_renders_the_line_number() {
        let error = StatusError::UnterminatedQuote { line: 7 };
        assert_eq!(error.to_string(), "line 7: unterminated quoted path");
    }
}
