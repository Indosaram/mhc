//! Port of `core/tools/diff-render.ts`.
use crate::jsdiff::diff_words;
use crate::theme::{get_language_from_path, highlight_code, Theme, ThemeBg, ThemeColor};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffKind {
    Added,
    Removed,
    Context,
    Meta,
}

#[derive(Clone, Debug)]
pub struct RenderableDiffLine {
    pub kind: DiffKind,
    pub content: String,
    pub line_number: String,
    pub sign: char,
    pub text: String,
}

fn parse_renderable_diff_line(line: &str) -> RenderableDiffLine {
    let meta = || RenderableDiffLine {
        kind: DiffKind::Meta,
        content: String::new(),
        line_number: String::new(),
        sign: ' ',
        text: line.to_owned(),
    };
    let mut chars = line.chars();
    let Some(sign) = chars.next() else {
        return meta();
    };
    if sign != '+' && sign != '-' && sign != ' ' {
        return meta();
    }
    let rest: String = chars.collect();
    let digits_end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
    let line_number = &rest[..digits_end];
    let Some(remainder) = rest.get(digits_end..) else {
        return meta();
    };
    if !remainder.starts_with(' ') {
        return meta();
    }
    let content = remainder[1..].to_owned();
    let kind = match sign {
        '+' => DiffKind::Added,
        '-' => DiffKind::Removed,
        _ => DiffKind::Context,
    };
    RenderableDiffLine { kind, content, line_number: line_number.to_owned(), sign, text: String::new() }
}

fn replace_tabs(text: &str) -> String {
    text.replace('\t', "   ")
}

fn highlight_diff_content(content: &str, file_path: Option<&str>, theme: &Theme) -> String {
    let plain_content = replace_tabs(content);
    let Some(file_path) = file_path else {
        return plain_content;
    };
    let Some(language) = get_language_from_path(file_path) else {
        return plain_content;
    };
    highlight_code(theme, &plain_content, Some(language))
        .first()
        .cloned()
        .unwrap_or(plain_content)
}

fn render_inline_diff(old_content: &str, new_content: &str, theme: &Theme) -> (String, String) {
    let parts = diff_words(&replace_tabs(old_content), &replace_tabs(new_content));
    let mut added = String::new();
    let mut removed = String::new();
    let mut first_added = true;
    let mut first_removed = true;

    for part in parts {
        if part.added {
            let mut value = part.value;
            if first_added {
                let leading = leading_whitespace(&value);
                added.push_str(&leading);
                value = value[leading.len()..].to_owned();
                first_added = false;
            }
            if !value.is_empty() {
                added.push_str(&theme.inverse(&value));
            }
            continue;
        }
        if part.removed {
            let mut value = part.value;
            if first_removed {
                let leading = leading_whitespace(&value);
                removed.push_str(&leading);
                value = value[leading.len()..].to_owned();
                first_removed = false;
            }
            if !value.is_empty() {
                removed.push_str(&theme.inverse(&value));
            }
            continue;
        }
        added.push_str(&part.value);
        removed.push_str(&part.value);
    }

    (added, removed)
}

fn leading_whitespace(text: &str) -> String {
    text.chars().take_while(|c| c.is_whitespace()).collect()
}

fn render_tool_diff_line(
    line: &RenderableDiffLine,
    file_path: Option<&str>,
    theme: &Theme,
    content_override: Option<&str>,
) -> String {
    let line_number = theme.fg(ThemeColor::Muted, &line.line_number);
    if line.kind == DiffKind::Context {
        return format!(
            "{}{} {}",
            theme.fg(ThemeColor::ToolDiffContext, &line.sign.to_string()),
            line_number,
            highlight_diff_content(&line.content, file_path, theme)
        );
    }

    let diff_color = if line.kind == DiffKind::Added { ThemeColor::ToolDiffAdded } else { ThemeColor::ToolDiffRemoved };
    let background = if line.kind == DiffKind::Added { ThemeBg::ToolSuccessBg } else { ThemeBg::ToolErrorBg };
    let content = match content_override {
        None => highlight_diff_content(&line.content, file_path, theme),
        Some(override_content) => theme.fg(diff_color, &replace_tabs(override_content)),
    };
    let rendered = format!("{}{} {}", theme.fg(diff_color, &line.sign.to_string()), line_number, content);
    theme.bg(background, &rendered)
}

pub fn render_tool_diff(diff_text: &str, file_path: Option<&str>, theme: &Theme) -> String {
    let parsed_lines: Vec<RenderableDiffLine> = diff_text.split('\n').map(parse_renderable_diff_line).collect();
    let mut rendered: Vec<String> = Vec::new();
    let mut index = 0;

    while index < parsed_lines.len() {
        let line = &parsed_lines[index];
        if line.kind != DiffKind::Removed {
            rendered.push(if line.kind == DiffKind::Meta {
                theme.fg(ThemeColor::ToolDiffContext, &line.text)
            } else {
                render_tool_diff_line(line, file_path, theme, None)
            });
            index += 1;
            continue;
        }

        let mut removed_lines: Vec<&RenderableDiffLine> = Vec::new();
        while parsed_lines.get(index).is_some_and(|line| line.kind == DiffKind::Removed) {
            if let Some(line) = parsed_lines.get(index) {
                removed_lines.push(line);
            }
            index += 1;
        }

        let mut added_lines: Vec<&RenderableDiffLine> = Vec::new();
        while parsed_lines.get(index).is_some_and(|line| line.kind == DiffKind::Added) {
            if let Some(line) = parsed_lines.get(index) {
                added_lines.push(line);
            }
            index += 1;
        }

        let paired_count = removed_lines.len().min(added_lines.len());
        for pair_index in 0..paired_count {
            let removed_line = removed_lines[pair_index];
            let added_line = added_lines[pair_index];
            let (added, removed) = render_inline_diff(&removed_line.content, &added_line.content, theme);
            rendered.push(render_tool_diff_line(removed_line, file_path, theme, Some(&removed)));
            rendered.push(render_tool_diff_line(added_line, file_path, theme, Some(&added)));
        }

        for removed_line in removed_lines.iter().skip(paired_count) {
            rendered.push(render_tool_diff_line(removed_line, file_path, theme, None));
        }
        for added_line in added_lines.iter().skip(paired_count) {
            rendered.push(render_tool_diff_line(added_line, file_path, theme, None));
        }
    }

    rendered.join("\n")
}
