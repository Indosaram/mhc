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

#[derive(Clone, Copy, PartialEq, Eq)]
enum CheckboxStatus {
    Open,
    Done,
    InProgress,
}

struct ParsedCheckbox<'a> {
    status: CheckboxStatus,
    label: &'a str,
}

#[derive(Clone, Copy)]
struct MarkdownFence {
    marker: char,
    length: usize,
}

struct ParsedStructuredPlan {
    checklist: PlanChecklist,
    next_task: Option<TopLevelTaskRef>,
    has_untracked_top_level_checkbox: bool,
}

/// Checklist of the plan file; a missing or unreadable plan yields an empty checklist.
pub fn get_plan_checklist(plan_path: &Path) -> PlanChecklist {
    match std::fs::read(plan_path) {
        Ok(bytes) => parse_plan_checklist(&String::from_utf8_lossy(&bytes)),
        Err(_) => empty_checklist(),
    }
}

/// Count top-level tasks: numbered `## TODOs` rows and `F<n>.` / `H<n>.` final-wave rows when a
/// structured heading exists, otherwise every top-level checkbox.
pub fn parse_plan_checklist(markdown: &str) -> PlanChecklist {
    let lines = split_lines(markdown);
    if !has_structured_section(&lines) {
        return parse_simple_checklist(&lines);
    }

    let structured_plan = parse_structured_plan(&lines);
    if structured_plan.checklist.total == 0 && structured_plan.has_untracked_top_level_checkbox {
        return parse_simple_checklist(&lines);
    }

    structured_plan.checklist
}

/// Whether `line` is a top-level structured task row counted in `section`.
pub fn is_structured_task_row(line: &str, section: TopLevelTaskSection) -> bool {
    parse_structured_top_level_checkbox(line, section).is_some()
}

pub(crate) fn parse_current_top_level_task(markdown: &str) -> Option<TopLevelTaskRef> {
    let lines = split_lines(markdown);
    if !has_structured_section(&lines) {
        return None;
    }

    parse_structured_plan(&lines).next_task
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

fn parse_structured_plan(lines: &[&str]) -> ParsedStructuredPlan {
    let mut completed = 0;
    let mut remaining = 0;
    let mut total = 0;
    let mut next_task_label = None;
    let mut next_task = None;
    let mut has_untracked_top_level_checkbox = false;
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
        if section == ChecklistSection::Other {
            if parse_simple_top_level_checkbox(line).is_some() {
                has_untracked_top_level_checkbox = true;
            }
            continue;
        }

        let task_section = match section {
            ChecklistSection::Todo => TopLevelTaskSection::Todo,
            ChecklistSection::FinalWave => TopLevelTaskSection::FinalWave,
            ChecklistSection::Other => unreachable!(),
        };
        let Some((checkbox, task)) = parse_structured_top_level_checkbox(line, task_section) else {
            continue;
        };

        total += 1;
        if checkbox.status == CheckboxStatus::Done {
            completed += 1;
            continue;
        }
        if checkbox.status == CheckboxStatus::InProgress {
            continue;
        }

        remaining += 1;
        if next_task_label.is_none() {
            next_task_label = Some(checkbox.label.to_string());
            next_task = Some(task);
        }
    }

    ParsedStructuredPlan {
        checklist: PlanChecklist {
            total,
            completed,
            remaining,
            next_task_label,
        },
        next_task,
        has_untracked_top_level_checkbox,
    }
}

fn parse_simple_checklist(lines: &[&str]) -> PlanChecklist {
    let mut completed = 0;
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
        if checkbox.status == CheckboxStatus::Done {
            completed += 1;
            continue;
        }
        if checkbox.status == CheckboxStatus::InProgress {
            continue;
        }

        remaining += 1;
        if next_task_label.is_none() {
            next_task_label = Some(checkbox.label.to_string());
        }
    }

    PlanChecklist {
        total,
        completed,
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

fn parse_checkbox_status(marker: char) -> CheckboxStatus {
    match marker {
        'x' | 'X' => CheckboxStatus::Done,
        '~' => CheckboxStatus::InProgress,
        _ => CheckboxStatus::Open,
    }
}

/// `^[-*][ \t]*\[[ \t]*([xX~]?)[ \t]*\][ \t]+(.+)$`
fn parse_simple_top_level_checkbox(line: &str) -> Option<ParsedCheckbox<'_>> {
    let rest = line.strip_prefix(['-', '*'])?;
    let rest = rest.trim_start_matches(is_blank).strip_prefix('[')?;
    let rest = rest.trim_start_matches(is_blank);
    let (status, rest) = match rest.chars().next() {
        Some(marker @ ('x' | 'X' | '~')) => (parse_checkbox_status(marker), &rest[marker.len_utf8()..]),
        _ => (CheckboxStatus::Open, rest),
    };
    let rest = rest.trim_start_matches(is_blank).strip_prefix(']')?;
    let label = rest.trim_start_matches(is_blank);
    let blank_run = rest.len() - label.len();
    if blank_run == 0 || !is_dot_text(rest) {
        return None;
    }
    if !label.is_empty() {
        return Some(ParsedCheckbox { status, label });
    }
    // `[ \t]+` backtracks one blank so `.+` can take the last one.
    (blank_run >= 2).then(|| ParsedCheckbox {
        status,
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

/// `^- \[([ xX~])\] (.+)$`
fn parse_structured_top_level_checkbox(
    line: &str,
    section: TopLevelTaskSection,
) -> Option<(ParsedCheckbox<'_>, TopLevelTaskRef)> {
    let rest = line.strip_prefix("- [")?;
    let mut characters = rest.chars();
    let marker = characters.next()?;
    let status = match marker {
        ' ' => CheckboxStatus::Open,
        'x' | 'X' => CheckboxStatus::Done,
        '~' => CheckboxStatus::InProgress,
        _ => return None,
    };
    let label = characters.as_str().strip_prefix("] ")?;
    if !is_dot_text(label) || label.is_empty() {
        return None;
    }
    let task = build_task_ref(section, label)?;
    Some((ParsedCheckbox { status, label }, task))
}

/// Parses the task key from `label`.
///
/// For `todo`: `^([1-9]\d*|T[1-9]\d*(?:\.[1-9]\d*[a-z]?)?)(?:\.[ \t]+|[ \t]+(?:[-\u2014][ \t]+)?)(.+)$`
/// For `final-wave`: `^([FH][1-9]\d*(?:\.[1-9]\d*[a-z]?)?)(?:\.[ \t]+|[ \t]+(?:[-\u2014][ \t]+)?)(.+)$`
fn build_task_ref(section: TopLevelTaskSection, label: &str) -> Option<TopLevelTaskRef> {
    let (raw_label, title) = parse_task_label(section, label)?;
    Some(TopLevelTaskRef {
        key: format!("{}:{}", section.as_str(), raw_label.to_lowercase()),
        section,
        label: raw_label.to_string(),
        title: title.to_string(),
    })
}

fn parse_task_label(section: TopLevelTaskSection, label: &str) -> Option<(&str, &str)> {
    let (raw_label, rest) = match section {
        TopLevelTaskSection::Todo => parse_todo_key_prefix(label)?,
        TopLevelTaskSection::FinalWave => parse_final_wave_key_prefix(label)?,
    };

    let title = parse_task_separator_and_title(rest)?;
    Some((raw_label, title))
}

/// `^([1-9]\d*|T[1-9]\d*(?:\.[1-9]\d*[a-z]?)?)` (case-insensitive)
fn parse_todo_key_prefix(label: &str) -> Option<(&str, &str)> {
    let bytes = label.as_bytes();
    if bytes.is_empty() {
        return None;
    }
    if bytes[0].eq_ignore_ascii_case(&b'T') {
        let mut idx = 1;
        let digits_start = idx;
        while idx < bytes.len() && bytes[idx].is_ascii_digit() {
            idx += 1;
        }
        let digits_len = idx - digits_start;
        if digits_len == 0 || bytes[digits_start] == b'0' {
            return None;
        }
        // Optional dotted subkey: .[1-9]d*[a-z]?
        if idx < bytes.len() && bytes[idx] == b'.' {
            let sub_start = idx + 1;
            let mut sub_idx = sub_start;
            while sub_idx < bytes.len() && bytes[sub_idx].is_ascii_digit() {
                sub_idx += 1;
            }
            let sub_digits_len = sub_idx - sub_start;
            if sub_digits_len > 0 && bytes[sub_start] != b'0' {
                if sub_idx < bytes.len() && bytes[sub_idx].is_ascii_alphabetic() {
                    sub_idx += 1;
                }
                idx = sub_idx;
            }
        }
        Some((&label[..idx], &label[idx..]))
    } else {
        // [1-9]d*
        let mut idx = 0;
        while idx < bytes.len() && bytes[idx].is_ascii_digit() {
            idx += 1;
        }
        if idx == 0 || bytes[0] == b'0' {
            return None;
        }
        Some((&label[..idx], &label[idx..]))
    }
}

/// `^([FH][1-9]\d*(?:\.[1-9]\d*[a-z]?)?)` (case-insensitive)
fn parse_final_wave_key_prefix(label: &str) -> Option<(&str, &str)> {
    let bytes = label.as_bytes();
    if bytes.is_empty() {
        return None;
    }
    let lead = bytes[0].to_ascii_uppercase();
    if lead != b'F' && lead != b'H' {
        return None;
    }

    let mut idx = 1;
    let digits_start = idx;
    while idx < bytes.len() && bytes[idx].is_ascii_digit() {
        idx += 1;
    }
    let digits_len = idx - digits_start;
    if digits_len == 0 || bytes[digits_start] == b'0' {
        return None;
    }
    // Optional dotted subkey: .[1-9]d*[a-z]?
    if idx < bytes.len() && bytes[idx] == b'.' {
        let sub_start = idx + 1;
        let mut sub_idx = sub_start;
        while sub_idx < bytes.len() && bytes[sub_idx].is_ascii_digit() {
            sub_idx += 1;
        }
        let sub_digits_len = sub_idx - sub_start;
        if sub_digits_len > 0 && bytes[sub_start] != b'0' {
            if sub_idx < bytes.len() && bytes[sub_idx].is_ascii_alphabetic() {
                sub_idx += 1;
            }
            idx = sub_idx;
        }
    }
    Some((&label[..idx], &label[idx..]))
}

/// Matches separator `(?:\.[ \t]+|[ \t]+(?:[-\u{2014}][ \t]+)?)` followed by `(.+)$`.
fn parse_task_separator_and_title(rest: &str) -> Option<&str> {
    if let Some(after_dot) = rest.strip_prefix('.') {
        let trimmed = after_dot.trim_start_matches(is_blank);
        let blank_count = after_dot.len() - trimmed.len();
        if blank_count == 0 {
            return None;
        }
        let title = if !trimmed.is_empty() {
            trimmed
        } else if blank_count >= 2 {
            &after_dot[blank_count - 1..]
        } else {
            return None;
        };
        if is_dot_text(title) {
            return Some(title);
        }
        return None;
    }

    // Must start with blank: `[ \t]+(?:[-\u{2014}][ \t]+)?`
    let after_blanks = rest.trim_start_matches(is_blank);
    let blank_count = rest.len() - after_blanks.len();
    if blank_count == 0 {
        return None;
    }

    // Check if dash or em-dash follows
    if let Some(after_dash) = after_blanks.strip_prefix('-').or_else(|| after_blanks.strip_prefix('—')) {
        let after_dash_blanks = after_dash.trim_start_matches(is_blank);
        let dash_blank_count = after_dash.len() - after_dash_blanks.len();
        if dash_blank_count > 0 {
            let title = if !after_dash_blanks.is_empty() {
                after_dash_blanks
            } else if dash_blank_count >= 2 {
                &after_dash[dash_blank_count - 1..]
            } else {
                // The optional dash group backtracks when it leaves no title.
                after_blanks
            };
            if is_dot_text(title) {
                return Some(title);
            }
            return None;
        }
    }

    let title = if !after_blanks.is_empty() {
        after_blanks
    } else if blank_count >= 2 {
        &rest[blank_count - 1..]
    } else {
        return None;
    };
    if is_dot_text(title) {
        return Some(title);
    }
    None
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
