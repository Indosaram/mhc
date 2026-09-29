//! Top-level plan checklist parsing (`plan-checklist.ts`).
//!
//! The TypeScript regular expressions are matched by hand so their exact JavaScript
//! semantics hold: `.` never matches `\r`, U+2028 or U+2029, `\d` is ASCII-only, and the
//! `i` flag folds ASCII letters only.

use std::path::Path;

use crate::types::{PlanChecklist, TopLevelTaskRef, TopLevelTaskSection};

#[derive(Clone, Copy, PartialEq, Eq)]
enum ChecklistSection {
    Todo,
    FinalWave,
    Other,
}

struct ParsedCheckbox<'a> {
    checked: bool,
    label: &'a str,
}

#[derive(Clone, Copy)]
struct MarkdownFence {
    marker: char,
    length: usize,
}

/// Checklist of the plan file; a missing or unreadable plan yields an empty checklist.
pub fn get_plan_checklist(plan_path: &Path) -> PlanChecklist {
    match std::fs::read(plan_path) {
        Ok(bytes) => parse_plan_checklist(&String::from_utf8_lossy(&bytes)),
        Err(_) => empty_checklist(),
    }
}

/// Count top-level tasks: numbered `## TODOs` rows and `F<n>.` final-wave rows when a
/// structured heading exists, otherwise every top-level checkbox.
pub fn parse_plan_checklist(markdown: &str) -> PlanChecklist {
    let lines = split_lines(markdown);
    if has_structured_section(&lines) {
        parse_structured_plan(&lines).0
    } else {
        parse_simple_checklist(&lines)
    }
}

pub(crate) fn parse_current_top_level_task(markdown: &str) -> Option<TopLevelTaskRef> {
    let lines = split_lines(markdown);
    if has_structured_section(&lines) {
        parse_structured_plan(&lines).1
    } else {
        None
    }
}

/// `markdown.split(/\r?\n/)`.
fn split_lines(markdown: &str) -> Vec<&str> {
    markdown
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .collect()
}

/// Tracks fenced code blocks; returns `true` when `line` is fence syntax or fenced content.
fn consume_fence(fence: &mut Option<MarkdownFence>, line: &str) -> bool {
    if let Some(open) = *fence {
        if is_closing_fence(line, open) {
            *fence = None;
        }
        return true;
    }
    if let Some(opening) = parse_opening_fence(line) {
        *fence = Some(opening);
        return true;
    }
    false
}

fn parse_structured_plan(lines: &[&str]) -> (PlanChecklist, Option<TopLevelTaskRef>) {
    let mut remaining = 0;
    let mut total = 0;
    let mut next_task_label = None;
    let mut next_task = None;
    let mut section = ChecklistSection::Other;
    let mut fence = None;

    for line in lines {
        if consume_fence(&mut fence, line) {
            continue;
        }
        if is_section_boundary(line) {
            section = parse_structured_section_heading(line);
            continue;
        }
        let task_section = match section {
            ChecklistSection::Todo => TopLevelTaskSection::Todo,
            ChecklistSection::FinalWave => TopLevelTaskSection::FinalWave,
            ChecklistSection::Other => continue,
        };
        let Some((checkbox, task)) = parse_structured_top_level_checkbox(line, task_section) else {
            continue;
        };
        total += 1;
        if checkbox.checked {
            continue;
        }
        remaining += 1;
        if next_task_label.is_none() {
            next_task_label = Some(checkbox.label.to_string());
            next_task = Some(task);
        }
    }

    let checklist = PlanChecklist {
        total,
        completed: total - remaining,
        remaining,
        next_task_label,
    };
    (checklist, next_task)
}

fn parse_simple_checklist(lines: &[&str]) -> PlanChecklist {
    let mut remaining = 0;
    let mut total = 0;
    let mut next_task_label = None;
    let mut fence = None;

    for line in lines {
        if consume_fence(&mut fence, line) {
            continue;
        }
        let Some(checkbox) = parse_simple_top_level_checkbox(line) else {
            continue;
        };
        total += 1;
        if checkbox.checked {
            continue;
        }
        remaining += 1;
        if next_task_label.is_none() {
            next_task_label = Some(checkbox.label.to_string());
        }
    }

    PlanChecklist {
        total,
        completed: total - remaining,
        remaining,
        next_task_label,
    }
}

fn is_blank(character: char) -> bool {
    character == ' ' || character == '\t'
}

/// Text a JavaScript `.+`/`.*` can consume up to `$`.
fn is_dot_text(text: &str) -> bool {
    !text.contains(['\r', '\n', '\u{2028}', '\u{2029}'])
}

/// `^[-*][ \t]*\[[ \t]*([xX]?)[ \t]*\][ \t]+(.+)$`
fn parse_simple_top_level_checkbox(line: &str) -> Option<ParsedCheckbox<'_>> {
    let rest = line.strip_prefix(['-', '*'])?;
    let rest = rest.trim_start_matches(is_blank).strip_prefix('[')?;
    let rest = rest.trim_start_matches(is_blank);
    let (checked, rest) = match rest.strip_prefix(['x', 'X']) {
        Some(after) => (true, after),
        None => (false, rest),
    };
    let rest = rest.trim_start_matches(is_blank).strip_prefix(']')?;
    let label = rest.trim_start_matches(is_blank);
    let blank_run = rest.len() - label.len();
    if blank_run == 0 || !is_dot_text(rest) {
        return None;
    }
    if !label.is_empty() {
        return Some(ParsedCheckbox { checked, label });
    }
    // `[ \t]+` backtracks one blank so `.+` can take the last one.
    (blank_run >= 2).then(|| ParsedCheckbox {
        checked,
        label: &rest[blank_run - 1..],
    })
}

fn has_structured_section(lines: &[&str]) -> bool {
    let mut fence = None;
    lines.iter().any(|line| {
        !consume_fence(&mut fence, line)
            && parse_structured_section_heading(line) != ChecklistSection::Other
    })
}

/// `^##[ \t]+<title>(?:[ \t]+#+)?[ \t]*$` with an ASCII case-insensitive title.
fn is_level_two_heading(line: &str, title: &str) -> bool {
    let Some(rest) = line.strip_prefix("##") else {
        return false;
    };
    let text = rest.trim_start_matches(is_blank);
    if text.len() == rest.len() || text.len() < title.len() || !text.is_char_boundary(title.len()) {
        return false;
    }
    let (candidate, tail) = text.split_at(title.len());
    if !candidate.eq_ignore_ascii_case(title) {
        return false;
    }
    let after_blanks = tail.trim_start_matches(is_blank);
    if after_blanks.is_empty() {
        return true;
    }
    if after_blanks.len() == tail.len() || !after_blanks.starts_with('#') {
        return false;
    }
    after_blanks
        .trim_start_matches('#')
        .trim_start_matches(is_blank)
        .is_empty()
}

fn parse_structured_section_heading(line: &str) -> ChecklistSection {
    if is_level_two_heading(line, "TODOs") {
        ChecklistSection::Todo
    } else if is_level_two_heading(line, "Final Verification Wave") {
        ChecklistSection::FinalWave
    } else {
        ChecklistSection::Other
    }
}

/// `^#{1,2}(?:[ \t]+|$)`
fn is_section_boundary(line: &str) -> bool {
    let hashes = line.len() - line.trim_start_matches('#').len();
    (1..=2).contains(&hashes) && line[hashes..].chars().next().is_none_or(is_blank)
}

/// `^- \[([ xX])\] (<label>)$` where the label is `[1-9]\d*\. .+` (todo) or
/// `F[1-9]\d*\. .+` case-insensitively (final wave).
fn parse_structured_top_level_checkbox(
    line: &str,
    section: TopLevelTaskSection,
) -> Option<(ParsedCheckbox<'_>, TopLevelTaskRef)> {
    let rest = line.strip_prefix("- [")?;
    let mut characters = rest.chars();
    let checked = match characters.next()? {
        ' ' => false,
        'x' | 'X' => true,
        _ => return None,
    };
    let label = characters.as_str().strip_prefix("] ")?;
    let task = build_task_ref(section, label)?;
    Some((ParsedCheckbox { checked, label }, task))
}

fn build_task_ref(section: TopLevelTaskSection, label: &str) -> Option<TopLevelTaskRef> {
    let number_start = match section {
        TopLevelTaskSection::Todo => 0,
        TopLevelTaskSection::FinalWave => {
            if !label.starts_with(['F', 'f']) {
                return None;
            }
            1
        }
    };
    let digits = label[number_start..]
        .bytes()
        .take_while(u8::is_ascii_digit)
        .count();
    let raw_end = number_start + digits;
    if digits == 0 || label.as_bytes()[number_start] == b'0' {
        return None;
    }
    let title = label[raw_end..].strip_prefix(". ")?;
    if title.is_empty() || !is_dot_text(title) {
        return None;
    }
    let raw_label = &label[..raw_end];
    Some(TopLevelTaskRef {
        key: format!("{}:{}", section.as_str(), raw_label.to_lowercase()),
        section,
        label: raw_label.to_string(),
        title: title.to_string(),
    })
}

/// Leading `[ \t]{0,3}` then a run of three or more backticks or tildes.
fn fence_run(line: &str) -> Option<(char, usize, &str)> {
    let indented = line.trim_start_matches(is_blank);
    if line.len() - indented.len() > 3 {
        return None;
    }
    let marker = indented.chars().next().filter(|c| *c == '`' || *c == '~')?;
    let after = indented.trim_start_matches(marker);
    let length = indented.len() - after.len();
    (length >= 3).then_some((marker, length, after))
}

/// `^[ \t]{0,3}(`{3,}|~{3,})(.*)$`; a backtick fence's info string may not contain a
/// backtick.
fn parse_opening_fence(line: &str) -> Option<MarkdownFence> {
    let (marker, length, info) = fence_run(line)?;
    if !is_dot_text(info) || (marker == '`' && info.contains('`')) {
        return None;
    }
    Some(MarkdownFence { marker, length })
}

/// `^[ \t]{0,3}(`{3,}|~{3,})[ \t]*$` with the opener's marker and at least its length.
fn is_closing_fence(line: &str, fence: MarkdownFence) -> bool {
    fence_run(line).is_some_and(|(marker, length, rest)| {
        marker == fence.marker
            && length >= fence.length
            && rest.trim_start_matches(is_blank).is_empty()
    })
}

fn empty_checklist() -> PlanChecklist {
    PlanChecklist {
        total: 0,
        completed: 0,
        remaining: 0,
        next_task_label: None,
    }
}
