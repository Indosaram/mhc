use crate::{
    line_endings::{LineReplacement, SourceText, replacements_around_context},
    seek_sequence::seek_sequence_with_fuzz,
    types::PatchChunk,
};
pub fn replace_chunks(
    content: &str,
    path: &str,
    chunks: &[PatchChunk],
) -> Result<(String, u32), String> {
    let source = SourceText::parse(content);
    let original = source.texts();
    let mut replacements: Vec<LineReplacement> = Vec::new();
    let mut line_index = 0;
    let mut fuzz = 0;
    for chunk in chunks {
        for context in &chunk.change_contexts {
            let Some((index, amount)) = seek_sequence_with_fuzz(
                &original,
                std::slice::from_ref(context),
                line_index,
                false,
            ) else {
                return Err(format!("Failed to find context '{context}' in {path}"));
            };
            fuzz += amount;
            line_index = index + 1;
        }
        if chunk.old_lines.is_empty() {
            let index = if original.last().is_some_and(String::is_empty) {
                original.len() - 1
            } else {
                original.len()
            };
            replacements.push(LineReplacement {
                start: index,
                old_length: 0,
                new_lines: chunk.new_lines.clone(),
            });
            continue;
        }
        let mut pattern = chunk.old_lines.as_slice();
        let mut new_lines = chunk.new_lines.as_slice();
        let mut found =
            seek_sequence_with_fuzz(&original, pattern, line_index, chunk.is_end_of_file);
        if found.is_none() && pattern.last().is_some_and(String::is_empty) {
            pattern = &pattern[..pattern.len() - 1];
            if new_lines.last().is_some_and(String::is_empty) {
                new_lines = &new_lines[..new_lines.len() - 1];
            }
            found = seek_sequence_with_fuzz(&original, pattern, line_index, chunk.is_end_of_file);
        }
        let Some((index, amount)) = found else {
            return Err(format!(
                "Failed to find expected lines in {path}:\n{}",
                chunk.old_lines.join("\n")
            ));
        };
        fuzz += amount;
        replacements.extend(replacements_around_context(
            index,
            pattern.len(),
            new_lines,
            &chunk.context_line_indices,
        ));
        line_index = index + pattern.len();
    }
    Ok((source.replace(&replacements), fuzz))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn applies_chunks_to_original_positions() {
        let chunks = [
            PatchChunk {
                old_lines: vec!["a".into()],
                new_lines: vec!["A".into(), "B".into()],
                ..Default::default()
            },
            PatchChunk {
                old_lines: vec!["b".into()],
                new_lines: vec!["C".into()],
                ..Default::default()
            },
        ];
        assert_eq!(
            replace_chunks("a\nb\n", "file", &chunks).unwrap(),
            ("A\nB\nC\n".into(), 0)
        );
    }
    #[test]
    fn context_skips_to_scope() {
        let chunk = PatchChunk {
            change_contexts: vec!["class".into()],
            old_lines: vec!["a".into()],
            new_lines: vec!["b".into()],
            ..Default::default()
        };
        assert_eq!(
            replace_chunks("a\nclass\na\n", "file", &[chunk]).unwrap().0,
            "a\nclass\nb\n"
        );
    }
    #[test]
    fn failed_lines() {
        let chunk = PatchChunk {
            old_lines: vec!["missing".into()],
            ..Default::default()
        };
        assert_eq!(
            replace_chunks("a", "file", &[chunk]).unwrap_err(),
            "Failed to find expected lines in file:\nmissing"
        );
    }
    #[test]
    fn trailing_empty_fallback() {
        let chunk = PatchChunk {
            old_lines: vec!["a".into(), "".into()],
            new_lines: vec!["b".into(), "".into()],
            ..Default::default()
        };
        assert_eq!(replace_chunks("a\n", "file", &[chunk]).unwrap().0, "b\n");
    }
    fn update_chunk(patch: &str) -> Vec<PatchChunk> {
        let parsed = crate::parser::parse_patch(patch).unwrap();
        let crate::types::ParsedPatch::Update { chunks, .. } = &parsed[0] else {
            panic!("expected update hunk")
        };
        chunks.clone()
    }
    #[test]
    fn keeps_crlf_on_every_line_for_change_and_insertion() {
        let chunks = update_chunk(
            "*** Begin Patch\n*** Update File: lines.txt\n@@\n-one\n+ONE\n two\n+between\n three\n*** End Patch\n",
        );
        assert_eq!(
            replace_chunks("one\r\ntwo\r\nthree\r\n", "lines.txt", &chunks)
                .unwrap()
                .0,
            "ONE\r\ntwo\r\nbetween\r\nthree\r\n"
        );
    }
    #[test]
    fn leaves_untouched_lines_of_mixed_ending_file_with_their_own_endings() {
        let chunks = update_chunk(
            "*** Begin Patch\n*** Update File: lines.txt\n@@\n one\n two\n-three\n+THREE\n four\n*** End Patch\n",
        );
        assert_eq!(
            replace_chunks("one\r\ntwo\rthree\nfour\r\n", "lines.txt", &chunks)
                .unwrap()
                .0,
            "one\r\ntwo\rTHREE\r\nfour\r\n"
        );
    }
    #[test]
    fn leaves_context_line_matched_only_after_trimming_exactly_as_it_was() {
        let chunks = update_chunk(
            "*** Begin Patch\n*** Update File: code.ts\n@@\n const a = 1;\n-const b = 2;\n+const b = 3;\n*** End Patch\n",
        );
        let (content, fuzz) =
            replace_chunks("const a = 1;   \r\nconst b = 2;\r\n", "code.ts", &chunks).unwrap();
        assert_eq!(content, "const a = 1;   \r\nconst b = 3;\r\n");
        assert!(fuzz > 0);
    }
    #[test]
    fn keeps_lf_file_lf_and_still_ends_with_newline() {
        let chunks = update_chunk(
            "*** Begin Patch\n*** Update File: lines.txt\n@@\n-beta\n+BETA\n*** End Patch\n",
        );
        assert_eq!(
            replace_chunks("alpha\nbeta", "lines.txt", &chunks)
                .unwrap()
                .0,
            "alpha\nBETA\n"
        );
    }
    #[test]
    fn duplicate_context_addition_keeps_crlf() {
        let chunks = update_chunk(
            "*** Begin Patch\n*** Update File: code.ts\n@@\n const a = 1;\n+const a = 1;\n const b = 2;\n*** End Patch",
        );
        assert_eq!(
            replace_chunks("const a = 1;\r\nconst b = 2;\r\n", "code.ts", &chunks)
                .unwrap()
                .0,
            "const a = 1;\r\nconst a = 1;\r\nconst b = 2;\r\n"
        );
    }
}
