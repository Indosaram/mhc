use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use unicode_normalization::UnicodeNormalization;

pub fn detect_line_ending(content: &str) -> &'static str {
    match (content.find("\r\n"), content.find('\n')) {
        (Some(crlf), Some(lf)) if crlf < lf => "\r\n",
        _ => "\n",
    }
}
pub fn normalize_to_lf(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}
pub fn restore_line_endings(text: &str, ending: &str) -> String {
    if ending == "\r\n" {
        text.replace('\n', "\r\n")
    } else {
        text.to_owned()
    }
}
pub fn normalize_for_fuzzy_match(text: &str) -> String {
    text.nfkc()
        .collect::<String>()
        .split('\n')
        .map(|line| line.trim_end_matches(|c: char| c.is_whitespace() || c == '\u{feff}'))
        .collect::<Vec<_>>()
        .join("\n")
        .chars()
        .map(|c| match c {
            '\u{2018}'..='\u{201b}' => '\'',
            '\u{201c}'..='\u{201f}' => '"',
            '\u{2010}'..='\u{2015}' | '\u{2212}' => '-',
            '\u{a0}' | '\u{2002}'..='\u{200a}' | '\u{202f}' | '\u{205f}' | '\u{3000}' => ' ',
            _ => c,
        })
        .collect()
}
#[derive(Debug, Clone)]
pub struct TextReplacement {
    pub match_index: usize,
    pub match_length: usize,
    pub new_text: String,
}
fn apply_replacements(content: &str, replacements: &[TextReplacement], offset: usize) -> String {
    let mut result = content.to_owned();
    for replacement in replacements.iter().rev() {
        let start = replacement.match_index - offset;
        result.replace_range(
            start..start + replacement.match_length,
            &replacement.new_text,
        );
    }
    result
}
pub fn apply_replacements_preserving_unchanged_lines(
    original: &str,
    base: &str,
    replacements: &[TextReplacement],
) -> Result<String, String> {
    let original_lines: Vec<_> = original.split_inclusive('\n').collect();
    let mut offset = 0;
    let spans: Vec<_> = base
        .split_inclusive('\n')
        .map(|line| {
            let start = offset;
            offset += line.len();
            (start, offset)
        })
        .collect();
    if original_lines.len() != spans.len() {
        return Err(
            "Cannot preserve unchanged lines because the base content has a different line count."
                .into(),
        );
    }
    let mut groups: Vec<(usize, usize, Vec<TextReplacement>)> = Vec::new();
    let mut sorted = replacements.to_vec();
    sorted.sort_by_key(|r| r.match_index);
    for replacement in sorted {
        let start = spans
            .iter()
            .position(|&(start, end)| {
                replacement.match_index >= start && replacement.match_index < end
            })
            .ok_or("Replacement range is outside the base content.")?;
        let end = spans
            .iter()
            .enumerate()
            .skip(start)
            .find(|(_, (_, end))| *end >= replacement.match_index + replacement.match_length)
            .map(|(i, _)| i + 1)
            .ok_or("Replacement range is outside the base content.")?;
        if let Some(group) = groups.last_mut().filter(|group| start < group.1) {
            group.1 = group.1.max(end);
            group.2.push(replacement);
        } else {
            groups.push((start, end, vec![replacement]));
        }
    }
    let mut cursor = 0;
    let mut result = String::new();
    for (start, end, replacements) in groups {
        result.push_str(&original_lines[cursor..start].concat());
        let offset = spans[start].0;
        result.push_str(&apply_replacements(
            &base[offset..spans[end - 1].1],
            &replacements,
            offset,
        ));
        cursor = end;
    }
    result.push_str(&original_lines[cursor..].concat());
    Ok(result)
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuzzyMatchResult {
    pub found: bool,
    pub index: Option<usize>,
    pub match_length: usize,
    pub used_fuzzy_match: bool,
    pub content_for_replacement: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Edit {
    pub old_text: String,
    pub new_text: String,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedEditsResult {
    pub base_content: String,
    pub new_content: String,
}
pub fn fuzzy_find_text(content: &str, old: &str) -> FuzzyMatchResult {
    if let Some(index) = content.find(old) {
        return FuzzyMatchResult {
            found: true,
            index: Some(index),
            match_length: old.len(),
            used_fuzzy_match: false,
            content_for_replacement: content.into(),
        };
    }
    let base = normalize_for_fuzzy_match(content);
    let old = normalize_for_fuzzy_match(old);
    match base.find(&old) {
        Some(index) => FuzzyMatchResult {
            found: true,
            index: Some(index),
            match_length: old.len(),
            used_fuzzy_match: true,
            content_for_replacement: base,
        },
        None => FuzzyMatchResult {
            found: false,
            index: None,
            match_length: 0,
            used_fuzzy_match: false,
            content_for_replacement: content.into(),
        },
    }
}
pub fn strip_bom(content: &str) -> (&str, &str) {
    content
        .strip_prefix('\u{feff}')
        .map_or(("", content), |text| ("\u{feff}", text))
}
pub fn apply_edits_to_normalized_content(
    content: &str,
    edits: &[Edit],
    path: &str,
) -> Result<AppliedEditsResult, String> {
    let edits: Vec<_> = edits
        .iter()
        .map(|edit| Edit {
            old_text: normalize_to_lf(&edit.old_text),
            new_text: normalize_to_lf(&edit.new_text),
        })
        .collect();
    for (i, edit) in edits.iter().enumerate() {
        if edit.old_text.is_empty() {
            return Err(if edits.len() == 1 {
                format!("oldText must not be empty in {path}.")
            } else {
                format!("edits[{i}].oldText must not be empty in {path}.")
            });
        }
    }
    let fuzzy = edits
        .iter()
        .any(|edit| fuzzy_find_text(content, &edit.old_text).used_fuzzy_match);
    let base = if fuzzy {
        normalize_for_fuzzy_match(content)
    } else {
        content.to_owned()
    };
    let mut matched = Vec::new();
    for (i, edit) in edits.iter().enumerate() {
        let found = fuzzy_find_text(&base, &edit.old_text);
        let Some(index) = found.index else {
            return Err(if edits.len() == 1 {
                format!(
                    "Could not find the exact text in {path}. The old text must match exactly including all whitespace and newlines."
                )
            } else {
                format!(
                    "Could not find edits[{i}] in {path}. The oldText must match exactly including all whitespace and newlines."
                )
            });
        };
        let normalized_base = normalize_for_fuzzy_match(&base);
        let old = normalize_for_fuzzy_match(&edit.old_text);
        let occurrences = if old.is_empty() {
            normalized_base.encode_utf16().count().saturating_sub(1)
        } else {
            normalized_base.matches(&old).count()
        };
        if occurrences > 1 {
            return Err(if edits.len() == 1 {
                format!(
                    "Found {occurrences} occurrences of the text in {path}. The text must be unique. Please provide more context to make it unique."
                )
            } else {
                format!(
                    "Found {occurrences} occurrences of edits[{i}] in {path}. Each oldText must be unique. Please provide more context to make it unique."
                )
            });
        }
        matched.push((
            i,
            TextReplacement {
                match_index: index,
                match_length: found.match_length,
                new_text: edit.new_text.clone(),
            },
        ));
    }
    matched.sort_by_key(|(_, r)| r.match_index);
    for pair in matched.windows(2) {
        if pair[0].1.match_index + pair[0].1.match_length > pair[1].1.match_index {
            return Err(format!(
                "edits[{}] and edits[{}] overlap in {path}. Merge them into one edit or target disjoint regions.",
                pair[0].0, pair[1].0
            ));
        }
    }
    let replacements: Vec<_> = matched.into_iter().map(|(_, r)| r).collect();
    let new_content = if fuzzy {
        apply_replacements_preserving_unchanged_lines(content, &base, &replacements)?
    } else {
        apply_replacements(&base, &replacements, 0)
    };
    if content == new_content {
        return Err(if edits.len() == 1 {
            format!(
                "No changes made to {path}. The replacement produced identical content. This might indicate an issue with special characters or the text not existing as expected."
            )
        } else {
            format!("No changes made to {path}. The replacements produced identical content.")
        });
    }
    Ok(AppliedEditsResult {
        base_content: content.to_owned(),
        new_content,
    })
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffResult {
    pub diff: String,
    pub first_changed_line: Option<usize>,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum ChangeTag {
    Equal,
    Delete,
    Insert,
}
#[derive(Clone)]
struct DiffPath {
    old_pos: isize,
    components: Vec<(ChangeTag, usize)>,
}
fn add_component(path: &mut DiffPath, tag: ChangeTag, count: usize) {
    if let Some((_, previous)) = path
        .components
        .last_mut()
        .filter(|(previous, _)| *previous == tag)
    {
        *previous += count;
    } else {
        path.components.push((tag, count));
    }
}
fn extract_common(path: &mut DiffPath, old: &[&str], new: &[&str], diagonal: isize) -> isize {
    let mut new_pos = path.old_pos - diagonal;
    let mut count = 0;
    while let (Ok(old_index), Ok(new_index)) = (
        usize::try_from(path.old_pos + 1),
        usize::try_from(new_pos + 1),
    ) {
        if old_index >= old.len() || new_index >= new.len() || old[old_index] != new[new_index] {
            break;
        }
        path.old_pos += 1;
        new_pos += 1;
        count += 1;
    }
    if count > 0 {
        add_component(path, ChangeTag::Equal, count);
    }
    new_pos
}
fn diff_lines<'a>(old: &'a str, new: &'a str) -> Vec<(ChangeTag, Vec<&'a str>)> {
    let old: Vec<_> = old.split_inclusive('\n').collect();
    let new: Vec<_> = new.split_inclusive('\n').collect();
    let mut initial = DiffPath {
        old_pos: -1,
        components: Vec::new(),
    };
    let position = extract_common(&mut initial, &old, &new, 0);
    let at_end = |path: &DiffPath, new_pos: isize| {
        usize::try_from(path.old_pos + 1).unwrap_or(0) >= old.len()
            && usize::try_from(new_pos + 1).unwrap_or(0) >= new.len()
    };
    let components = if at_end(&initial, position) {
        initial.components
    } else {
        let mut paths = BTreeMap::from([(0isize, initial)]);
        let mut minimum = isize::MIN;
        let mut maximum = isize::MAX;
        let mut completed = Vec::new();
        'search: for length in 1..=old.len() + new.len() {
            let length = isize::try_from(length).unwrap_or(isize::MAX);
            let mut diagonal = minimum.max(-length);
            while diagonal <= maximum.min(length) {
                let remove = paths.remove(&(diagonal - 1));
                let add = paths.get(&(diagonal + 1));
                let can_add = add.is_some_and(|path| {
                    usize::try_from(path.old_pos - diagonal).is_ok_and(|pos| pos < new.len())
                });
                let can_remove = remove.as_ref().is_some_and(|path| {
                    usize::try_from(path.old_pos + 1).is_ok_and(|pos| pos < old.len())
                });
                if !can_add && !can_remove {
                    paths.remove(&diagonal);
                    diagonal += 2;
                    continue;
                }
                let use_add = !can_remove
                    || can_add
                        && remove
                            .as_ref()
                            .zip(add)
                            .is_some_and(|(remove, add)| remove.old_pos < add.old_pos);
                let candidate = if use_add {
                    add.cloned().map(|mut path| {
                        add_component(&mut path, ChangeTag::Insert, 1);
                        path
                    })
                } else {
                    remove.map(|mut path| {
                        path.old_pos += 1;
                        add_component(&mut path, ChangeTag::Delete, 1);
                        path
                    })
                };
                if let Some(mut path) = candidate {
                    let new_pos = extract_common(&mut path, &old, &new, diagonal);
                    if at_end(&path, new_pos) {
                        completed = path.components;
                        break 'search;
                    }
                    if usize::try_from(path.old_pos + 1).unwrap_or(0) >= old.len() {
                        maximum = maximum.min(diagonal - 1);
                    }
                    if usize::try_from(new_pos + 1).unwrap_or(0) >= new.len() {
                        minimum = minimum.max(diagonal + 1);
                    }
                    paths.insert(diagonal, path);
                }
                diagonal += 2;
            }
        }
        completed
    };
    let (mut old_pos, mut new_pos) = (0, 0);
    components
        .into_iter()
        .map(|(tag, count)| {
            let lines = match tag {
                ChangeTag::Delete => {
                    let lines = old[old_pos..old_pos + count].to_vec();
                    old_pos += count;
                    lines
                }
                ChangeTag::Insert => {
                    let lines = new[new_pos..new_pos + count].to_vec();
                    new_pos += count;
                    lines
                }
                ChangeTag::Equal => {
                    let lines = new[new_pos..new_pos + count].to_vec();
                    old_pos += count;
                    new_pos += count;
                    lines
                }
            };
            (tag, lines)
        })
        .collect()
}
pub fn generate_diff_string(old: &str, new: &str, context: usize) -> DiffResult {
    let parts = diff_lines(old, new);
    let width = old
        .split('\n')
        .count()
        .max(new.split('\n').count())
        .to_string()
        .len();
    let (mut old_num, mut new_num) = (1, 1);
    let mut first = None;
    let mut output = Vec::new();
    for (i, (tag, lines)) in parts.iter().enumerate() {
        match tag {
            ChangeTag::Delete | ChangeTag::Insert => {
                first.get_or_insert(new_num);
                for line in lines {
                    let line = line.strip_suffix('\n').unwrap_or(line);
                    if *tag == ChangeTag::Insert {
                        output.push(format!("+{new_num:width$} {line}"));
                        new_num += 1;
                    } else {
                        output.push(format!("-{old_num:width$} {line}"));
                        old_num += 1;
                    }
                }
            }
            ChangeTag::Equal => {
                let leading = i > 0;
                let trailing = i + 1 < parts.len();
                let mut skipping = false;
                for (j, line) in lines.iter().enumerate() {
                    let line = line.strip_suffix('\n').unwrap_or(line);
                    let show = leading && j < context
                        || trailing && j >= lines.len().saturating_sub(context);
                    if show {
                        output.push(format!(" {old_num:width$} {line}"));
                        skipping = false;
                    } else if !skipping && (leading || trailing) {
                        output.push(format!(" {:width$} ...", ""));
                        skipping = true;
                    }
                    old_num += 1;
                    new_num += 1;
                }
            }
        }
    }
    DiffResult {
        diff: output.join("\n"),
        first_changed_line: first,
    }
}
pub fn generate_unified_patch(path: &str, old: &str, new: &str, context: usize) -> String {
    let mut parts = diff_lines(old, new);
    parts.push((ChangeTag::Equal, Vec::new()));
    let quoted = quote_filename(path);
    let mut output = format!("--- {quoted}\n+++ {quoted}\n");
    let (mut old_line, mut new_line) = (1, 1);
    let mut start = None;
    let mut range: Vec<(char, &str)> = Vec::new();
    for (i, (tag, lines)) in parts.iter().enumerate() {
        match tag {
            ChangeTag::Insert | ChangeTag::Delete => {
                if start.is_none() {
                    if let Some((_, previous)) = i.checked_sub(1).and_then(|i| parts.get(i)) {
                        range.extend(
                            previous
                                .iter()
                                .skip(previous.len().saturating_sub(context))
                                .map(|line| (' ', *line)),
                        );
                    }
                    start = Some((old_line - range.len(), new_line - range.len()));
                }
                range.extend(
                    lines
                        .iter()
                        .map(|line| (if *tag == ChangeTag::Insert { '+' } else { '-' }, *line)),
                );
                if *tag == ChangeTag::Insert {
                    new_line += lines.len();
                } else {
                    old_line += lines.len();
                }
            }
            ChangeTag::Equal => {
                if let Some((old_start, new_start)) = start {
                    if lines.len() <= context.saturating_mul(2) && i < parts.len().saturating_sub(2)
                    {
                        range.extend(lines.iter().map(|line| (' ', *line)));
                    } else {
                        let size = lines.len().min(context);
                        range.extend(lines.iter().take(size).map(|line| (' ', *line)));
                        let old_count = old_line - old_start + size;
                        let new_count = new_line - new_start + size;
                        let old_start = if old_count == 0 {
                            old_start - 1
                        } else {
                            old_start
                        };
                        let new_start = if new_count == 0 {
                            new_start - 1
                        } else {
                            new_start
                        };
                        output.push_str(&format!(
                            "@@ -{old_start},{old_count} +{new_start},{new_count} @@\n"
                        ));
                        for (prefix, line) in range.drain(..) {
                            output.push(prefix);
                            output.push_str(line);
                            if !line.ends_with('\n') {
                                output.push_str("\n\\ No newline at end of file\n");
                            }
                        }
                        start = None;
                    }
                }
                old_line += lines.len();
                new_line += lines.len();
            }
        }
    }
    output
}
fn quote_filename(path: &str) -> String {
    if path
        .bytes()
        .all(|b| (0x20..=0x7e).contains(&b) && b != b'"' && b != b'\\')
    {
        return path.into();
    }
    let mut output = String::from("\"");
    for byte in path.bytes() {
        match byte {
            7 => output.push_str("\\a"),
            8 => output.push_str("\\b"),
            9 => output.push_str("\\t"),
            10 => output.push_str("\\n"),
            11 => output.push_str("\\v"),
            12 => output.push_str("\\f"),
            13 => output.push_str("\\r"),
            b'"' => output.push_str("\\\""),
            b'\\' => output.push_str("\\\\"),
            0x20..=0x7e => output.push(char::from(byte)),
            _ => output.push_str(&format!("\\{byte:03o}")),
        }
    }
    output.push('"');
    output
}
