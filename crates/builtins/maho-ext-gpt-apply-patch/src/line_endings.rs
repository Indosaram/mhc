//! Port of `gpt-apply-patch/line-endings.ts` (senpi 903b8336).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LineEnding {
    Lf,
    CrLf,
    Cr,
}

impl LineEnding {
    fn as_str(self) -> &'static str {
        match self {
            Self::Lf => "\n",
            Self::CrLf => "\r\n",
            Self::Cr => "\r",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SourceLine {
    text: String,
    ending: Option<LineEnding>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineReplacement {
    pub start: usize,
    pub old_length: usize,
    pub new_lines: Vec<String>,
}

/// `[index in oldLines, index in newLines]` of a context line, which sits on both sides of a chunk.
pub type ContextLineIndex = (usize, usize);

/// Splits a matched chunk into replacements that skip its context lines, so those source lines keep
/// their exact text and ending (Codex `compute_replacements`, PreserveLineEndings mode). Context
/// indices past the matched region belong to a trailing empty line the matcher already dropped.
pub fn replacements_around_context(
    start: usize,
    old_length: usize,
    new_lines: &[String],
    context_line_indices: &[ContextLineIndex],
) -> Vec<LineReplacement> {
    let mut replacements = Vec::new();
    let mut old_start = 0;
    let mut new_start = 0;
    for &(old_context, new_context) in context_line_indices {
        if old_context >= old_length || new_context >= new_lines.len() {
            break;
        }
        if old_start != old_context || new_start != new_context {
            replacements.push(LineReplacement {
                start: start + old_start,
                old_length: old_context - old_start,
                new_lines: new_lines[new_start..new_context].to_vec(),
            });
        }
        old_start = old_context + 1;
        new_start = new_context + 1;
    }
    if old_start != old_length || new_start != new_lines.len() {
        replacements.push(LineReplacement {
            start: start + old_start,
            old_length: old_length - old_start,
            new_lines: new_lines[new_start..].to_vec(),
        });
    }
    replacements
}

/// A file split into lines that remember their own line ending, so an update rewrites only the lines a
/// patch touches. Mirrors Codex `codex-rs/apply-patch/src/text_file.rs`: unchanged lines keep their
/// endings, inserted lines take the file's first ending (LF when it has none), and every line ends with
/// an ending, which keeps apply_patch's trailing-newline behavior.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceText {
    lines: Vec<SourceLine>,
    preferred_ending: LineEnding,
}

impl SourceText {
    pub fn parse(content: &str) -> Self {
        let bytes = content.as_bytes();
        let mut lines = Vec::new();
        let mut preferred_ending = None;
        let mut line_start = 0;
        let mut cursor = 0;
        while cursor < bytes.len() {
            let character = bytes[cursor];
            if character != b'\n' && character != b'\r' {
                cursor += 1;
                continue;
            }
            let ending = if character == b'\r' && bytes.get(cursor + 1) == Some(&b'\n') {
                LineEnding::CrLf
            } else if character == b'\r' {
                LineEnding::Cr
            } else {
                LineEnding::Lf
            };
            preferred_ending.get_or_insert(ending);
            lines.push(SourceLine {
                text: content[line_start..cursor].to_owned(),
                ending: Some(ending),
            });
            cursor += ending.as_str().len();
            line_start = cursor;
        }
        if line_start < content.len() {
            lines.push(SourceLine {
                text: content[line_start..].to_owned(),
                ending: None,
            });
        }
        Self {
            lines,
            preferred_ending: preferred_ending.unwrap_or(LineEnding::Lf),
        }
    }

    pub fn texts(&self) -> Vec<String> {
        self.lines.iter().map(|line| line.text.clone()).collect()
    }

    pub fn replace(&self, replacements: &[LineReplacement]) -> String {
        let mut sorted: Vec<&LineReplacement> = replacements.iter().collect();
        sorted.sort_by_key(|replacement| replacement.start);
        let mut next: Vec<SourceLine> = Vec::new();
        let mut source_index = 0;
        for replacement in sorted {
            // Array.slice clamps; an overlapping or out-of-range start never panics upstream.
            let end = replacement.start.min(self.lines.len());
            if source_index < end {
                next.extend_from_slice(&self.lines[source_index..end]);
            }
            next.extend(replacement.new_lines.iter().map(|text| SourceLine {
                text: text.clone(),
                ending: Some(self.preferred_ending),
            }));
            source_index = replacement.start + replacement.old_length;
        }
        if source_index < self.lines.len() {
            next.extend_from_slice(&self.lines[source_index..]);
        }
        next.iter()
            .map(|line| {
                format!(
                    "{}{}",
                    line.text,
                    line.ending.unwrap_or(self.preferred_ending).as_str()
                )
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn parse_keeps_each_line_ending() {
        let source = SourceText::parse("a\r\nb\nc\rd");
        assert_eq!(source.texts(), lines(&["a", "b", "c", "d"]));
        assert_eq!(source.replace(&[]), "a\r\nb\nc\rd\r\n");
    }

    #[test]
    fn inserted_lines_take_the_first_ending() {
        let source = SourceText::parse("one\r\ntwo\nthree\n");
        let output = source.replace(&[LineReplacement {
            start: 1,
            old_length: 1,
            new_lines: lines(&["TWO", "extra"]),
        }]);
        assert_eq!(output, "one\r\nTWO\r\nextra\r\nthree\n");
    }

    #[test]
    fn file_without_endings_defaults_to_lf() {
        let source = SourceText::parse("only");
        assert_eq!(source.texts(), lines(&["only"]));
        assert_eq!(
            source.replace(&[LineReplacement {
                start: 0,
                old_length: 1,
                new_lines: lines(&["new"]),
            }]),
            "new\n"
        );
        assert_eq!(SourceText::parse("").replace(&[]), "");
    }

    #[test]
    fn replacements_apply_in_start_order() {
        let source = SourceText::parse("a\nb\nc\n");
        let output = source.replace(&[
            LineReplacement {
                start: 2,
                old_length: 1,
                new_lines: lines(&["C"]),
            },
            LineReplacement {
                start: 0,
                old_length: 1,
                new_lines: lines(&["A"]),
            },
        ]);
        assert_eq!(output, "A\nb\nC\n");
    }

    #[test]
    fn replacements_skip_context_lines() {
        // old: [ctx, x, ctx2], new: [ctx, y, z, ctx2]
        let replacements = replacements_around_context(
            5,
            3,
            &lines(&["ctx", "y", "z", "ctx2"]),
            &[(0, 0), (2, 3)],
        );
        assert_eq!(
            replacements,
            vec![LineReplacement {
                start: 6,
                old_length: 1,
                new_lines: lines(&["y", "z"]),
            }]
        );
    }

    #[test]
    fn trailing_change_after_last_context_is_kept() {
        let replacements =
            replacements_around_context(0, 2, &lines(&["ctx", "new", "tail"]), &[(0, 0)]);
        assert_eq!(
            replacements,
            vec![LineReplacement {
                start: 1,
                old_length: 1,
                new_lines: lines(&["new", "tail"]),
            }]
        );
    }

    #[test]
    fn context_past_matched_region_is_ignored() {
        // A trailing empty context line the matcher dropped sits past oldLength.
        let replacements = replacements_around_context(0, 1, &lines(&["b"]), &[(1, 1)]);
        assert_eq!(
            replacements,
            vec![LineReplacement {
                start: 0,
                old_length: 1,
                new_lines: lines(&["b"]),
            }]
        );
    }

    #[test]
    fn crlf_file_keeps_untouched_context_endings() {
        let source = SourceText::parse("keep\r\nold\nlast\r\n");
        let replacements = replacements_around_context(0, 2, &lines(&["keep", "new"]), &[(0, 0)]);
        assert_eq!(source.replace(&replacements), "keep\r\nnew\r\nlast\r\n");
    }
}
