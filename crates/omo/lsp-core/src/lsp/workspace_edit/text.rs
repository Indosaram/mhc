//! Port of TS `workspace-edit-text.ts`. Characters are UTF-16 code units, as in LSP.

use super::types::ParsedTextEdit;
use super::types::WorkspaceEditValidationError as VErr;
use std::cmp::Ordering;

/// TS `NormalizedTextEditResult`.
#[derive(Debug, Clone, PartialEq)]
pub struct NormalizedTextEditResult {
    pub edits: Vec<ParsedTextEdit>,
    pub text: String,
}

fn compare(left: (f64, f64), right: (f64, f64)) -> Ordering {
    let difference = if left.0 == right.0 {
        left.1 - right.1
    } else {
        left.0 - right.0
    };
    difference.partial_cmp(&0.0).unwrap_or(Ordering::Equal)
}

fn start(edit: &ParsedTextEdit) -> (f64, f64) {
    (edit.start_line, edit.start_character)
}

fn end(edit: &ParsedTextEdit) -> (f64, f64) {
    (edit.end_line, edit.end_character)
}

fn same_range(left: &ParsedTextEdit, right: &ParsedTextEdit) -> bool {
    start(left) == start(right) && end(left) == end(right)
}

/// TS `formatRange`: 1-based `l:c-l:c`.
pub fn format_range(edit: &ParsedTextEdit) -> String {
    format!(
        "{}:{}-{}:{}",
        edit.start_line + 1.0,
        edit.start_character + 1.0,
        edit.end_line + 1.0,
        edit.end_character + 1.0
    )
}

fn validate_position(
    position: (f64, f64),
    label: &str,
    lines: &[Vec<u16>],
    change_index: usize,
) -> Result<(), VErr> {
    let (line, character) = position;
    if line.fract() != 0.0
        || character.fract() != 0.0
        || !line.is_finite()
        || !character.is_finite()
    {
        return Err(VErr::new(
            change_index,
            format!("{label} position must use integer line and character"),
        ));
    }
    if line < 0.0 || character < 0.0 {
        return Err(VErr::new(
            change_index,
            format!("{label} position cannot be negative"),
        ));
    }
    let Some(text) = lines.get(line as usize) else {
        return Err(VErr::new(
            change_index,
            format!("{label} line {line} is outside the document"),
        ));
    };
    if character as usize > text.len() {
        return Err(VErr::new(
            change_index,
            format!("{label} character {character} is outside line {line}"),
        ));
    }
    Ok(())
}

fn split_lines(content: &str) -> Vec<Vec<u16>> {
    content
        .split('\n')
        .map(|line| line.encode_utf16().collect())
        .collect()
}

/// TS `normalizeTextEdits`: validate, sort descending, dedupe, reject overlap, apply.
pub fn normalize_text_edits(
    content: &str,
    edits: &[ParsedTextEdit],
    change_index: usize,
) -> Result<NormalizedTextEditResult, VErr> {
    let lines = split_lines(content);
    for edit in edits {
        validate_position(start(edit), "start", &lines, change_index)?;
        validate_position(end(edit), "end", &lines, change_index)?;
        if compare(start(edit), end(edit)) == Ordering::Greater {
            return Err(VErr::new(
                change_index,
                format!("range {} ends before it starts", format_range(edit)),
            ));
        }
    }
    let mut indexed: Vec<(usize, &ParsedTextEdit)> = edits.iter().enumerate().collect();
    indexed
        .sort_by(|left, right| compare(start(right.1), start(left.1)).then(right.0.cmp(&left.0)));
    let mut unique: Vec<ParsedTextEdit> = Vec::new();
    for (_, edit) in indexed {
        let empty = start(edit) == end(edit);
        if let Some(previous) = unique.last()
            && !empty
            && same_range(previous, edit)
            && previous.new_text == edit.new_text
        {
            continue;
        }
        unique.push(edit.clone());
    }
    for pair in unique.windows(2) {
        let (later, earlier) = (&pair[0], &pair[1]);
        if compare(end(earlier), start(later)) == Ordering::Greater {
            return Err(VErr::new(
                change_index,
                format!(
                    "overlapping edits {} and {}",
                    format_range(earlier),
                    format_range(later)
                ),
            ));
        }
    }
    let text = apply_normalized(content, &unique);
    Ok(NormalizedTextEditResult {
        edits: unique,
        text,
    })
}

fn apply_normalized(content: &str, edits: &[ParsedTextEdit]) -> String {
    let mut lines = split_lines(content);
    for edit in edits {
        let (start_line, start_character) =
            (edit.start_line as usize, edit.start_character as usize);
        let (end_line, end_character) = (edit.end_line as usize, edit.end_character as usize);
        let (Some(first), Some(last)) = (lines.get(start_line), lines.get(end_line)) else {
            continue;
        };
        let mut replacement: Vec<u16> = first[..start_character.min(first.len())].to_vec();
        replacement.extend(edit.new_text.encode_utf16());
        replacement.extend_from_slice(&last[end_character.min(last.len())..]);
        let replacement_lines = replacement
            .split(|unit| *unit == u16::from(b'\n'))
            .map(<[u16]>::to_vec);
        lines.splice(start_line..=end_line, replacement_lines);
    }
    let joined: Vec<u16> = lines.join(&u16::from(b'\n'));
    String::from_utf16_lossy(&joined)
}
