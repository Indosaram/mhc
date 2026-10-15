//! Exact and fuzzy disjoint replacements, and theme-free diff computation.
use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;
use crate::definition::ToolError;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Edit { pub old_text: String, pub new_text: String }
#[derive(Debug, PartialEq, Eq)]
pub struct AppliedEditsResult { pub base_content: String, pub new_content: String }
#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EditDiffResult { pub diff: String, pub first_changed_line: Option<usize> }
pub fn detect_line_ending(content: &str) -> &'static str {
    match (content.find("\r\n"), content.find('\n')) {
        (Some(crlf), Some(lf)) if crlf < lf => "\r\n",
        _ => "\n",
    }
}
pub fn normalize_to_lf(content: &str) -> String { content.replace("\r\n", "\n").replace('\r', "\n") }
pub fn restore_line_endings(content: &str, ending: &str) -> String {
    if ending == "\r\n" { content.replace('\n', "\r\n") } else { content.into() }
}
pub fn normalize_for_fuzzy_match(content: &str) -> String {
    content.nfkc().collect::<String>().split('\n').map(str::trim_end).collect::<Vec<_>>().join("\n")
        .chars().map(|c| match c {
            '\u{2018}'..='\u{201b}' => '\'',
            '\u{201c}'..='\u{201f}' => '"',
            '\u{2010}'..='\u{2015}' | '\u{2212}' => '-',
            '\u{a0}' | '\u{2002}'..='\u{200a}' | '\u{202f}' | '\u{205f}' | '\u{3000}' => ' ',
            _ => c,
        }).collect()
}
#[derive(Debug)]
pub struct FuzzyMatchResult {
    pub found: bool,
    pub index: Option<usize>,
    pub match_length: usize,
    pub used_fuzzy_match: bool,
    pub content_for_replacement: String,
}
pub fn fuzzy_find_text(content: &str, old_text: &str) -> FuzzyMatchResult {
    if let Some(index) = content.find(old_text) {
        return FuzzyMatchResult { found: true, index: Some(index), match_length: old_text.len(), used_fuzzy_match: false, content_for_replacement: content.into() };
    }
    let normalized = normalize_for_fuzzy_match(content);
    let old = normalize_for_fuzzy_match(old_text);
    match normalized.find(&old) {
        Some(index) => FuzzyMatchResult { found: true, index: Some(index), match_length: old.len(), used_fuzzy_match: true, content_for_replacement: normalized },
        None => FuzzyMatchResult { found: false, index: None, match_length: 0, used_fuzzy_match: false, content_for_replacement: content.into() },
    }
}
#[derive(Clone, Debug)]
pub struct TextReplacement { pub edit_index: usize, pub match_index: usize, pub match_length: usize, pub new_text: String }
fn apply_replacements(content: &str, replacements: &[TextReplacement], offset: usize) -> String {
    let mut result = content.to_owned();
    for replacement in replacements.iter().rev() {
        let start = replacement.match_index - offset;
        result.replace_range(start..start + replacement.match_length, &replacement.new_text);
    }
    result
}
pub fn apply_replacements_preserving_unchanged_lines(original: &str, base: &str, replacements: &[TextReplacement]) -> Result<String, ToolError> {
    let original_lines: Vec<_> = original.split_inclusive('\n').collect();
    let base_lines: Vec<_> = base.split_inclusive('\n').collect();
    if original_lines.len() != base_lines.len() {
        return Err(ToolError::Message("Cannot preserve unchanged lines because the base content has a different line count.".into()));
    }
    let mut spans = Vec::new();
    let mut offset = 0;
    for line in &base_lines { spans.push((offset, offset + line.len())); offset += line.len(); }
    let mut sorted = replacements.to_vec(); sorted.sort_by_key(|r| r.match_index);
    let mut groups: Vec<(usize, usize, Vec<TextReplacement>)> = Vec::new();
    for replacement in sorted {
        let start = spans.iter().position(|(start, end)| replacement.match_index >= *start && replacement.match_index < *end)
            .ok_or_else(|| ToolError::Message("Replacement range is outside the base content.".into()))?;
        let end = spans.iter().enumerate().skip(start).find(|(_, (_, end))| *end >= replacement.match_index + replacement.match_length)
            .map(|(index, _)| index + 1).ok_or_else(|| ToolError::Message("Replacement range is outside the base content.".into()))?;
        if let Some(group) = groups.last_mut().filter(|g| start < g.1) {
            group.1 = group.1.max(end); group.2.push(replacement);
        } else { groups.push((start, end, vec![replacement])); }
    }
    let mut result = String::new(); let mut cursor = 0;
    for (start, end, replacements) in groups {
        result.push_str(&original_lines[cursor..start].concat());
        let offset = spans[start].0;
        result.push_str(&apply_replacements(&base[offset..spans[end - 1].1], &replacements, offset));
        cursor = end;
    }
    result.push_str(&original_lines[cursor..].concat()); Ok(result)
}
pub fn apply_edits_to_normalized_content(content: &str, edits: &[Edit], path: &str) -> Result<AppliedEditsResult, ToolError> {
    let edits: Vec<_> = edits.iter().map(|e| Edit { old_text: normalize_to_lf(&e.old_text), new_text: normalize_to_lf(&e.new_text) }).collect();
    let count = edits.len();
    for (index, edit) in edits.iter().enumerate() {
        if edit.old_text.is_empty() { return Err(ToolError::Message(if count == 1 { format!("oldText must not be empty in {path}.") } else { format!("edits[{index}].oldText must not be empty in {path}.") })); }
    }
    let fuzzy = edits.iter().any(|e| fuzzy_find_text(content, &e.old_text).used_fuzzy_match);
    let base = if fuzzy { normalize_for_fuzzy_match(content) } else { content.into() };
    let normalized_base = normalize_for_fuzzy_match(&base);
    let mut matched = Vec::new();
    for (index, edit) in edits.iter().enumerate() {
        let found = fuzzy_find_text(&base, &edit.old_text);
        let Some(at) = found.index else { return Err(ToolError::Message(if count == 1 {
            format!("Could not find the exact text in {path}. The old text must match exactly including all whitespace and newlines.")
        } else { format!("Could not find edits[{index}] in {path}. The oldText must match exactly including all whitespace and newlines.") })); };
        let occurrences = normalized_base.matches(&normalize_for_fuzzy_match(&edit.old_text)).count();
        if occurrences > 1 { return Err(ToolError::Message(if count == 1 {
            format!("Found {occurrences} occurrences of the text in {path}. The text must be unique. Please provide more context to make it unique.")
        } else { format!("Found {occurrences} occurrences of edits[{index}] in {path}. Each oldText must be unique. Please provide more context to make it unique.") })); }
        matched.push(TextReplacement { edit_index: index, match_index: at, match_length: found.match_length, new_text: edit.new_text.clone() });
    }
    matched.sort_by_key(|r| r.match_index);
    for pair in matched.windows(2) {
        if pair[0].match_index + pair[0].match_length > pair[1].match_index {
            return Err(ToolError::Message(format!("edits[{}] and edits[{}] overlap in {path}. Merge them into one edit or target disjoint regions.", pair[0].edit_index, pair[1].edit_index)));
        }
    }
    let new_content = if fuzzy { apply_replacements_preserving_unchanged_lines(content, &base, &matched)? } else { apply_replacements(&base, &matched, 0) };
    if content == new_content { return Err(ToolError::Message(if count == 1 {
        format!("No changes made to {path}. The replacement produced identical content. This might indicate an issue with special characters or the text not existing as expected.")
    } else { format!("No changes made to {path}. The replacements produced identical content.") })); }
    Ok(AppliedEditsResult { base_content: content.into(), new_content })
}

/// One forward Myers step `d` over `v`; returns true when the end point is reached.
fn myers_step(v: &mut [usize], d: usize, offset: usize, a: &[&str], b: &[&str]) -> bool {
    for step in (0..=2*d).step_by(2) {
        let k = offset + step - d;
        let mut x = if step == 0 || (step != 2*d && v[k-1] < v[k+1]) { v[k+1] } else { v[k-1]+1 };
        let mut y = x + d - step;
        while x < a.len() && y < b.len() && a[x] == b[y] { x += 1; y += 1; }
        v[k] = x;
        if x == a.len() && y == b.len() { return true; }
    }
    false
}
/// Myers shortest edit script over newline-preserving tokens. Deletions precede additions.
///
/// The backtrack needs the `v` state before every step. Instead of keeping all D snapshots
/// (O(D*(N+M)) memory), only every `interval`-th snapshot band is kept and each block is
/// replayed during the backtrack; the replay is deterministic, so the script is unchanged.
pub(crate) fn diff_lines<'a>(old: &'a str, new: &'a str) -> Vec<(char, &'a str)> {
    let a: Vec<_> = old.split_inclusive('\n').collect();
    let b: Vec<_> = new.split_inclusive('\n').collect();
    let max = a.len() + b.len();
    let offset = max + 1;
    // Before step d only diagonals offset-d+1..=offset+d-1 are written; the backtrack reads one past each edge.
    let band = |v: &[usize], d: usize| v[offset-d-1..=offset+d+1].to_vec();
    let mut interval = 1;
    while interval * interval < max + 1 { interval += 1; }
    let mut v = vec![0usize; 2 * max + 3];
    let mut checkpoints = Vec::new();
    let mut distance = 0;
    for d in 0..=max {
        if d % interval == 0 { checkpoints.push(band(&v, d)); }
        if myers_step(&mut v, d, offset, &a, &b) { distance = d; break; }
    }
    let (mut x, mut y) = (a.len(), b.len());
    let mut edits = Vec::new();
    for (block, checkpoint) in checkpoints.iter().enumerate().rev() {
        let start = block * interval;
        let end = (start + interval - 1).min(distance);
        v.fill(0);
        v[offset-start-1..=offset+start+1].copy_from_slice(checkpoint);
        let mut trace = Vec::with_capacity(end - start + 1);
        for d in start..=end {
            trace.push(band(&v, d));
            if d < end { myers_step(&mut v, d, offset, &a, &b); }
        }
        for d in (start..=end).rev() {
            let snapshot = &trace[d - start];
            let lo = offset - d - 1;
            let at = |index: usize| snapshot[index - lo];
            let k = offset + x - y;
            let previous_k = if k == offset-d || (k != offset+d && at(k-1) < at(k+1)) { k+1 } else { k-1 };
            let previous_x = at(previous_k);
            let previous_y = if previous_k >= offset { previous_x.saturating_sub(previous_k-offset) } else { previous_x + offset-previous_k };
            while x > previous_x && y > previous_y { x -= 1; y -= 1; edits.push((' ', a[x])); }
            if d > 0 {
                if x == previous_x { y -= 1; edits.push(('+', b[y])); }
                else { x -= 1; edits.push(('-', a[x])); }
            }
        }
    }
    edits.reverse();
    // jsdiff groups removals before additions within each changed block.
    let mut index = 0;
    while index < edits.len() {
        if edits[index].0 == ' ' { index += 1; continue; }
        let end = edits[index..].iter().position(|e| e.0 == ' ').map_or(edits.len(), |n| index+n);
        edits[index..end].sort_by_key(|e| if e.0 == '-' { 0 } else { 1 }); index = end;
    }
    edits
}
pub fn generate_diff_string(old: &str, new: &str, context: usize) -> EditDiffResult {
    generate_diff_string_from_rows(&diff_lines(old, new), old, new, context)
}
/// [`generate_diff_string`] over rows already computed by [`diff_lines`] for the same inputs.
pub(crate) fn generate_diff_string_from_rows(rows: &[(char, &str)], old: &str, new: &str, context: usize) -> EditDiffResult {
    let width = old.split('\n').count().max(new.split('\n').count()).to_string().len();
    let mut output = Vec::new(); let (mut old_line, mut new_line) = (1,1); let mut first = None; let mut index = 0;
    while index < rows.len() {
        let (kind, line) = rows[index];
        if kind != ' ' {
            first.get_or_insert(new_line);
            let number = if kind == '+' { new_line } else { old_line };
            output.push(format!("{kind}{number:>width$} {}", line.strip_suffix('\n').unwrap_or(line)));
            if kind == '+' { new_line += 1; } else { old_line += 1; }
            index += 1; continue;
        }
        let end = rows[index..].iter().position(|r| r.0 != ' ').map_or(rows.len(), |n| index+n);
        let leading = index > 0; let trailing = end < rows.len(); let length = end-index;
        let mut elided = false;
        for (relative, (_, line)) in rows[index..end].iter().enumerate() {
            let show = (leading && relative < context) || (trailing && relative >= length.saturating_sub(context));
            if show { output.push(format!(" {old_line:>width$} {}", line.strip_suffix('\n').unwrap_or(line))); elided = false; }
            else if (leading || trailing) && !elided { output.push(format!(" {:>width$} ...", "")); elided = true; }
            old_line += 1; new_line += 1;
        }
        index = end;
    }
    EditDiffResult { diff: output.join("\n"), first_changed_line: first }
}
pub fn generate_unified_patch(path: &str, old: &str, new: &str, context: usize) -> String {
    crate::unified_diff::create_unified_patch(path, path, old, new, context)
}
pub async fn compute_edits_diff(path: &str, edits: &[Edit], cwd: &std::path::Path) -> Result<EditDiffResult, ToolError> {
    let text = tokio::fs::read_to_string(crate::path_utils::resolve_to_cwd(path, cwd)).await?;
    let normalized = normalize_to_lf(text.strip_prefix('\u{feff}').unwrap_or(&text));
    let result = apply_edits_to_normalized_content(&normalized, edits, path)?;
    Ok(generate_diff_string(&result.base_content, &result.new_content, 4))
}

#[cfg(test)]
mod tests {
    use super::diff_lines;

    /// The previous full-trace implementation, kept as the oracle for `diff_lines`.
    fn reference_diff_lines<'a>(old: &'a str, new: &'a str) -> Vec<(char, &'a str)> {
        let a: Vec<_> = old.split_inclusive('\n').collect();
        let b: Vec<_> = new.split_inclusive('\n').collect();
        let max = a.len() + b.len();
        let offset = max + 1;
        let mut v = vec![0usize; 2 * max + 3];
        let mut trace = Vec::new();
        let mut distance = 0;
        'search: for d in 0..=max {
            trace.push(v.clone());
            for step in (0..=2*d).step_by(2) {
                let k = offset + step - d;
                let mut x = if step == 0 || (step != 2*d && v[k-1] < v[k+1]) { v[k+1] } else { v[k-1]+1 };
                let mut y = x + d - step;
                while x < a.len() && y < b.len() && a[x] == b[y] { x += 1; y += 1; }
                v[k] = x;
                if x == a.len() && y == b.len() { distance = d; break 'search; }
            }
        }
        let (mut x, mut y) = (a.len(), b.len());
        let mut edits = Vec::new();
        for d in (0..=distance).rev() {
            let v = &trace[d];
            let k = offset + x - y;
            let previous_k = if k == offset-d || (k != offset+d && v[k-1] < v[k+1]) { k+1 } else { k-1 };
            let previous_x = v[previous_k];
            let previous_y = if previous_k >= offset { previous_x.saturating_sub(previous_k-offset) } else { previous_x + offset-previous_k };
            while x > previous_x && y > previous_y { x -= 1; y -= 1; edits.push((' ', a[x])); }
            if d > 0 {
                if x == previous_x { y -= 1; edits.push(('+', b[y])); }
                else { x -= 1; edits.push(('-', a[x])); }
            }
        }
        edits.reverse();
        let mut index = 0;
        while index < edits.len() {
            if edits[index].0 == ' ' { index += 1; continue; }
            let end = edits[index..].iter().position(|e| e.0 == ' ').map_or(edits.len(), |n| index+n);
            edits[index..end].sort_by_key(|e| if e.0 == '-' { 0 } else { 1 }); index = end;
        }
        edits
    }

    fn pseudo_random_text(seed: u64, lines: usize, alphabet: u64) -> String {
        let mut state = seed;
        (0..lines).map(|_| {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            format!("line {}\n", (state >> 33) % alphabet)
        }).collect()
    }

    #[test]
    fn checkpointed_diff_matches_full_trace_reference() {
        let full_old: String = (0..200).map(|i| format!("old {i}\n")).collect();
        let full_new: String = (0..150).map(|i| format!("new {i}\n")).collect();
        let mut cases: Vec<(String, String)> = vec![
            (String::new(), String::new()),
            (String::new(), "a\nb".into()),
            ("a\nb".into(), String::new()),
            ("a\n".into(), "a\na\n".into()),
            ("a\nb\nc\n".into(), "a\nx\nc\n".into()),
            ("a\nb\nc".into(), "a\nb\nc\n".into()),
            (full_old.clone(), full_new.clone()),
            (full_old, String::new()),
            (String::new(), full_new),
        ];
        for seed in 0..24 {
            let old = pseudo_random_text(seed, 40 + (seed as usize * 7) % 90, 3 + seed % 6);
            let new = pseudo_random_text(seed + 1000, 30 + (seed as usize * 11) % 100, 3 + seed % 6);
            cases.push((old, new));
        }
        for (old, new) in &cases {
            assert_eq!(diff_lines(old, new), reference_diff_lines(old, new), "old={old:?} new={new:?}");
        }
    }
}
