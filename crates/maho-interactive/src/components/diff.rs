//! Port of `components/diff.ts`.
use crate::jsdiff::diff_words;
use crate::theme::{Theme, ThemeColor};

pub const LONG_LINE_FAST_PATH_LIMIT: usize = 500;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IntraLineDiff {
    pub removed_line: String,
    pub added_line: String,
}

fn parse_diff_line(line: &str) -> Option<(char, String, String)> {
    let mut chars = line.chars();
    let prefix = chars.next()?;
    if prefix != '+' && prefix != '-' && prefix != ' ' {
        return None;
    }
    let rest: String = chars.collect();
    let digits_end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
    let line_num = rest[..digits_end].to_owned();
    let remainder = &rest[digits_end..];
    if !remainder.starts_with(' ') {
        return None;
    }
    Some((prefix, line_num, remainder[1..].to_owned()))
}

fn replace_tabs(text: &str) -> String {
    text.replace('\t', "   ")
}

pub fn render_intra_line_diff_with_diff_words(old_content: &str, new_content: &str, theme: &Theme) -> IntraLineDiff {
    let word_diff = diff_words(old_content, new_content);

    let mut removed_line = String::new();
    let mut added_line = String::new();
    let mut is_first_removed = true;
    let mut is_first_added = true;

    for part in word_diff {
        if part.removed {
            let mut value = part.value;
            if is_first_removed {
                let leading_ws = leading_whitespace(&value);
                value = value[leading_ws.len()..].to_owned();
                removed_line.push_str(&leading_ws);
                is_first_removed = false;
            }
            if !value.is_empty() {
                removed_line.push_str(&theme.inverse(&value));
            }
        } else if part.added {
            let mut value = part.value;
            if is_first_added {
                let leading_ws = leading_whitespace(&value);
                value = value[leading_ws.len()..].to_owned();
                added_line.push_str(&leading_ws);
                is_first_added = false;
            }
            if !value.is_empty() {
                added_line.push_str(&theme.inverse(&value));
            }
        } else {
            removed_line.push_str(&part.value);
            added_line.push_str(&part.value);
        }
    }

    IntraLineDiff { removed_line, added_line }
}

fn leading_whitespace(text: &str) -> String {
    text.chars().take_while(|c| c.is_whitespace()).collect()
}

pub fn render_intra_line_diff(old_content: &str, new_content: &str, theme: &Theme) -> IntraLineDiff {
    render_intra_line_diff_fast_path(old_content, new_content, theme)
        .unwrap_or_else(|| render_intra_line_diff_with_diff_words(old_content, new_content, theme))
}

pub fn render_intra_line_diff_fast_path(old_content: &str, new_content: &str, theme: &Theme) -> Option<IntraLineDiff> {
    if old_content == new_content {
        return Some(IntraLineDiff {
            removed_line: old_content.to_owned(),
            added_line: new_content.to_owned(),
        });
    }
    render_single_span_intra_line_diff(old_content, new_content, theme)
}

fn render_single_span_intra_line_diff(old_content: &str, new_content: &str, theme: &Theme) -> Option<IntraLineDiff> {
    let span = find_single_diff_words_replacement(old_content, new_content)?;
    Some(IntraLineDiff {
        removed_line: format!("{}{}{}", span.prefix, theme.inverse(&span.removed), span.suffix),
        added_line: format!("{}{}{}", span.prefix, theme.inverse(&span.added), span.suffix),
    })
}

struct ReplacementSpan {
    prefix: String,
    removed: String,
    added: String,
    suffix: String,
}

fn find_single_diff_words_replacement(old_content: &str, new_content: &str) -> Option<ReplacementSpan> {
    let old: Vec<char> = old_content.chars().collect();
    let new: Vec<char> = new_content.chars().collect();

    let mut start = 0usize;
    while start < old.len() && start < new.len() && old[start] == new[start] {
        start += 1;
    }

    let mut old_end = old.len();
    let mut new_end = new.len();
    while old_end > start && new_end > start && old[old_end - 1] == new[new_end - 1] {
        old_end -= 1;
        new_end -= 1;
    }

    while start > 0
        && (is_ascii_word_code(old.get(start - 1).copied().unwrap_or_default())
            || is_ascii_word_code(new.get(start - 1).copied().unwrap_or_default()))
    {
        start -= 1;
    }
    while old_end < old.len()
        && new_end < new.len()
        && (is_ascii_word_code(old.get(old_end).copied().unwrap_or_default())
            || is_ascii_word_code(new.get(new_end).copied().unwrap_or_default()))
    {
        old_end += 1;
        new_end += 1;
    }

    let prefix: String = old[..start].iter().collect();
    let removed: String = old[start..old_end].iter().collect();
    let added: String = new[start..new_end].iter().collect();
    let old_suffix: String = old[old_end..].iter().collect();
    let new_suffix: String = new[new_end..].iter().collect();

    if old_suffix != new_suffix {
        return None;
    }
    if !is_single_diff_words_replacement(&removed, &added) {
        return None;
    }
    Some(ReplacementSpan { prefix, removed, added, suffix: old_suffix })
}

fn is_single_diff_words_replacement(removed: &str, added: &str) -> bool {
    !removed.is_empty() && !added.is_empty() && is_simple_diff_token(removed) && is_simple_diff_token(added)
}

fn is_simple_diff_token(value: &str) -> bool {
    value.chars().all(is_ascii_word_code)
}

fn is_ascii_word_code(code: char) -> bool {
    code.is_ascii_alphanumeric() || code == '_'
}

pub fn render_diff(diff_text: &str, theme: &Theme) -> String {
    let lines: Vec<&str> = diff_text.split('\n').collect();
    let mut result: Vec<String> = Vec::new();

    let mut index = 0;
    while index < lines.len() {
        let line = lines[index];
        let Some((prefix, line_num, content)) = parse_diff_line(line) else {
            result.push(theme.fg(ThemeColor::ToolDiffContext, line));
            index += 1;
            continue;
        };

        if prefix == '-' {
            let mut removed_lines: Vec<(String, String)> = Vec::new();
            while index < lines.len() {
                let Some((prefix, line_num, content)) = parse_diff_line(lines[index]) else {
                    break;
                };
                if prefix != '-' {
                    break;
                }
                removed_lines.push((line_num, content));
                index += 1;
            }

            let mut added_lines: Vec<(String, String)> = Vec::new();
            while index < lines.len() {
                let Some((prefix, line_num, content)) = parse_diff_line(lines[index]) else {
                    break;
                };
                if prefix != '+' {
                    break;
                }
                added_lines.push((line_num, content));
                index += 1;
            }

            if removed_lines.len() == 1 && added_lines.len() == 1 {
                let (removed_num, removed_content) = &removed_lines[0];
                let (added_num, added_content) = &added_lines[0];
                let intra = render_intra_line_diff(&replace_tabs(removed_content), &replace_tabs(added_content), theme);
                result.push(theme.fg(ThemeColor::ToolDiffRemoved, &format!("-{removed_num} {}", intra.removed_line)));
                result.push(theme.fg(ThemeColor::ToolDiffAdded, &format!("+{added_num} {}", intra.added_line)));
            } else {
                for (line_num, content) in &removed_lines {
                    result.push(theme.fg(ThemeColor::ToolDiffRemoved, &format!("-{line_num} {}", replace_tabs(content))));
                }
                for (line_num, content) in &added_lines {
                    result.push(theme.fg(ThemeColor::ToolDiffAdded, &format!("+{line_num} {}", replace_tabs(content))));
                }
            }
        } else if prefix == '+' {
            result.push(theme.fg(ThemeColor::ToolDiffAdded, &format!("+{line_num} {}", replace_tabs(&content))));
            index += 1;
        } else {
            result.push(theme.fg(ThemeColor::ToolDiffContext, &format!(" {line_num} {}", replace_tabs(&content))));
            index += 1;
        }
    }

    result.join("\n")
}
