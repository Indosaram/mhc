use super::scan::{Scanner, TokenKind, is_eol};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsoncEdit {
    pub offset: usize,
    pub length: usize,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormattingOptions {
    pub eol: String,
    pub insert_spaces: bool,
    pub tab_size: usize,
    pub keep_lines: bool,
    pub insert_final_newline: bool,
}

impl Default for FormattingOptions {
    fn default() -> Self {
        Self {
            eol: "\n".to_string(),
            insert_spaces: true,
            tab_size: 2,
            keep_lines: false,
            insert_final_newline: false,
        }
    }
}

impl FormattingOptions {
    fn tab_size_or_default(&self) -> usize {
        if self.tab_size == 0 { 4 } else { self.tab_size }
    }

    fn indent_value(&self) -> String {
        if self.insert_spaces {
            " ".repeat(self.tab_size_or_default())
        } else {
            "\t".to_string()
        }
    }
}

pub fn apply_edit(text: &str, edit: &JsoncEdit) -> String {
    let mut out = String::with_capacity(text.len() + edit.content.len());
    out.push_str(&text[..edit.offset]);
    out.push_str(&edit.content);
    out.push_str(&text[edit.offset + edit.length..]);
    out
}

pub fn apply_edits(text: &str, edits: &[JsoncEdit]) -> Result<String, JsoncEditError> {
    let mut sorted: Vec<&JsoncEdit> = edits.iter().collect();
    sorted.sort_by(|a, b| {
        a.offset
            .cmp(&b.offset)
            .then_with(|| a.length.cmp(&b.length))
    });

    let mut current = text.to_string();
    let mut last_modified_offset = text.len();
    for edit in sorted.iter().rev() {
        if edit.offset + edit.length <= last_modified_offset {
            current = apply_edit(&current, edit);
        } else {
            return Err(JsoncEditError::OverlappingEdit);
        }
        last_modified_offset = edit.offset;
    }
    Ok(current)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JsoncEditError {
    OverlappingEdit,
    DeleteInEmptyDocument,
    CannotModifyParent,
}

impl JsoncEditError {
    pub fn message(&self) -> &'static str {
        match self {
            JsoncEditError::OverlappingEdit => "Overlapping edit",
            JsoncEditError::DeleteInEmptyDocument => "Cannot delete in an empty document",
            JsoncEditError::CannotModifyParent => "Cannot modify the parent of a missing node",
        }
    }
}

impl std::fmt::Display for JsoncEditError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.message())
    }
}

pub fn format_range(
    document: &str,
    range: Option<(usize, usize)>,
    options: &FormattingOptions,
) -> Vec<JsoncEdit> {
    let (range_start, range_end, format_start, format_text) = match range {
        Some((offset, length)) => {
            let range_start = offset;
            let range_end = offset + length;
            let mut format_start = range_start;
            while format_start > 0 && !is_eol(document, format_start - 1) {
                format_start -= 1;
            }
            let mut end_offset = range_end;
            while end_offset < document.len() && !is_eol(document, end_offset) {
                end_offset += 1;
            }
            (
                Some(range_start),
                Some(range_end),
                format_start,
                &document[format_start..end_offset],
            )
        }
        None => (None, None, 0, document),
    };

    let initial_indent_level = compute_indent_level(format_text, options) as i64;
    let eol = get_eol(options, document);
    let indent_value = options.indent_value();
    let mut indent_level: i64 = 0;
    let mut edits: Vec<JsoncEdit> = Vec::new();
    let mut scanner = Scanner::new(format_text);

    let mut scan_next = |number_line_breaks: &mut usize| -> (TokenKind, usize, usize, bool) {
        let mut token = scanner.scan();
        let mut line_breaks = 0;
        let mut has_error = false;
        loop {
            if token.kind == TokenKind::Trivia || token.kind == TokenKind::LineBreak {
                if token.kind == TokenKind::LineBreak {
                    line_breaks = if options.keep_lines {
                        line_breaks + 1
                    } else {
                        1
                    };
                }
            } else {
                break;
            }
            has_error |= scanner.error_count() > 0;
            token = scanner.scan();
        }
        *number_line_breaks = line_breaks;
        (token.kind, token.offset, token.length, has_error)
    };

    let add_edit =
        |edits: &mut Vec<JsoncEdit>, content: &str, start: usize, end: usize, has_error: bool| {
            if has_error {
                return;
            }
            if let (Some(range_start), Some(range_end)) = (range_start, range_end)
                && !(start < range_end && end > range_start)
            {
                return;
            }
            if &document[start..end] == content {
                return;
            }
            edits.push(JsoncEdit {
                offset: start,
                length: end - start,
                content: content.to_string(),
            });
        };

    let mut number_line_breaks = 0usize;
    let (mut first_kind, mut first_offset, mut first_length, mut has_error) =
        scan_next(&mut number_line_breaks);
    if options.keep_lines && number_line_breaks > 0 {
        add_edit(&mut edits, &eol.repeat(number_line_breaks), 0, 0, has_error);
    }
    if first_kind != TokenKind::Eof {
        let first_token_start = first_offset + format_start;
        let initial_indent = indent_value.repeat(initial_indent_level.max(0) as usize);
        add_edit(
            &mut edits,
            &initial_indent,
            format_start,
            first_token_start,
            has_error,
        );
    }

    let _ = first_length;
    while first_kind != TokenKind::Eof {
        let first_token_end = first_offset + first_length + format_start;
        let (mut second_kind, mut second_offset, second_length, second_has_error) =
            scan_next(&mut number_line_breaks);
        let mut replace_content = String::new();
        let mut needs_line_break = false;
        while number_line_breaks == 0
            && matches!(
                second_kind,
                TokenKind::LineComment | TokenKind::BlockComment
            )
        {
            let comment_start = second_offset + format_start;
            add_edit(&mut edits, " ", first_token_end, comment_start, has_error);
            has_error |= second_has_error;
            let comment_end = second_offset + second_length + format_start;
            needs_line_break = second_kind == TokenKind::LineComment;
            replace_content = if needs_line_break {
                new_lines_and_indent(
                    number_line_breaks,
                    indent_level,
                    initial_indent_level,
                    &indent_value,
                    &eol,
                )
            } else {
                String::new()
            };
            let (kind, offset, length, errored) = scan_next(&mut number_line_breaks);
            second_kind = kind;
            second_offset = offset;
            let _ = length;
            has_error |= errored;
            let _ = comment_end;
        }

        if second_kind == TokenKind::CloseBrace {
            if first_kind != TokenKind::OpenBrace {
                indent_level -= 1;
            }
            if !options.keep_lines && first_kind != TokenKind::OpenBrace {
                replace_content = new_lines_and_indent(
                    number_line_breaks,
                    indent_level,
                    initial_indent_level,
                    &indent_value,
                    &eol,
                );
            } else if options.keep_lines {
                replace_content = " ".to_string();
            }
        } else if second_kind == TokenKind::CloseBracket {
            if first_kind != TokenKind::OpenBracket {
                indent_level -= 1;
            }
            if !options.keep_lines && first_kind != TokenKind::OpenBracket {
                replace_content = new_lines_and_indent(
                    number_line_breaks,
                    indent_level,
                    initial_indent_level,
                    &indent_value,
                    &eol,
                );
            } else if options.keep_lines {
                replace_content = " ".to_string();
            }
        } else {
            match first_kind {
                TokenKind::OpenBracket | TokenKind::OpenBrace => {
                    indent_level += 1;
                    if !options.keep_lines || number_line_breaks > 0 {
                        replace_content = new_lines_and_indent(
                            number_line_breaks,
                            indent_level,
                            initial_indent_level,
                            &indent_value,
                            &eol,
                        );
                    } else {
                        replace_content = " ".to_string();
                    }
                }
                TokenKind::Comma => {
                    if !options.keep_lines || number_line_breaks > 0 {
                        replace_content = new_lines_and_indent(
                            number_line_breaks,
                            indent_level,
                            initial_indent_level,
                            &indent_value,
                            &eol,
                        );
                    } else {
                        replace_content = " ".to_string();
                    }
                }
                TokenKind::LineComment => {
                    replace_content = new_lines_and_indent(
                        number_line_breaks,
                        indent_level,
                        initial_indent_level,
                        &indent_value,
                        &eol,
                    );
                }
                TokenKind::BlockComment => {
                    if number_line_breaks > 0 {
                        replace_content = new_lines_and_indent(
                            number_line_breaks,
                            indent_level,
                            initial_indent_level,
                            &indent_value,
                            &eol,
                        );
                    } else if !needs_line_break {
                        replace_content = " ".to_string();
                    }
                }
                TokenKind::Colon => {
                    if options.keep_lines && number_line_breaks > 0 {
                        replace_content = new_lines_and_indent(
                            number_line_breaks,
                            indent_level,
                            initial_indent_level,
                            &indent_value,
                            &eol,
                        );
                    } else if !needs_line_break {
                        replace_content = " ".to_string();
                    }
                }
                TokenKind::String => {
                    if options.keep_lines && number_line_breaks > 0 {
                        replace_content = new_lines_and_indent(
                            number_line_breaks,
                            indent_level,
                            initial_indent_level,
                            &indent_value,
                            &eol,
                        );
                    } else if second_kind == TokenKind::Colon && !needs_line_break {
                        replace_content = String::new();
                    }
                }
                TokenKind::Null
                | TokenKind::True
                | TokenKind::False
                | TokenKind::Number
                | TokenKind::CloseBrace
                | TokenKind::CloseBracket => {
                    if options.keep_lines && number_line_breaks > 0 {
                        replace_content = new_lines_and_indent(
                            number_line_breaks,
                            indent_level,
                            initial_indent_level,
                            &indent_value,
                            &eol,
                        );
                    } else if matches!(
                        second_kind,
                        TokenKind::LineComment | TokenKind::BlockComment
                    ) && !needs_line_break
                    {
                        replace_content = " ".to_string();
                    } else if second_kind != TokenKind::Comma && second_kind != TokenKind::Eof {
                        has_error = true;
                    }
                }
                TokenKind::Unknown => has_error = true,
                _ => {}
            }
            if number_line_breaks > 0
                && matches!(
                    second_kind,
                    TokenKind::LineComment | TokenKind::BlockComment
                )
            {
                replace_content = new_lines_and_indent(
                    number_line_breaks,
                    indent_level,
                    initial_indent_level,
                    &indent_value,
                    &eol,
                );
            }
        }

        if second_kind == TokenKind::Eof {
            replace_content = if options.keep_lines && number_line_breaks > 0 {
                new_lines_and_indent(
                    number_line_breaks,
                    indent_level,
                    initial_indent_level,
                    &indent_value,
                    &eol,
                )
            } else if options.insert_final_newline {
                eol.clone()
            } else {
                String::new()
            };
        }

        let second_token_start = second_offset + format_start;
        add_edit(
            &mut edits,
            &replace_content,
            first_token_end,
            second_token_start,
            has_error,
        );
        first_kind = second_kind;
        first_offset = second_offset;
        first_length = second_length;
    }

    edits
}

pub fn new_lines_and_indent(
    number_line_breaks: usize,
    indent_level: i64,
    initial_indent_level: i64,
    indent_value: &str,
    eol: &str,
) -> String {
    let level = (initial_indent_level + indent_level).max(0) as usize;
    if number_line_breaks > 1 {
        format!(
            "{}{}",
            eol.repeat(number_line_breaks),
            indent_value.repeat(level)
        )
    } else if level == 0 {
        eol.to_string()
    } else {
        format!("{}{}", eol, indent_value.repeat(level))
    }
}

pub fn compute_indent_level(content: &str, options: &FormattingOptions) -> usize {
    let tab_size = options.tab_size_or_default();
    let mut chars = 0usize;
    for ch in content.chars() {
        if ch == ' ' {
            chars += 1;
        } else if ch == '\t' {
            chars += tab_size;
        } else {
            break;
        }
    }
    chars / tab_size
}

pub fn get_eol(options: &FormattingOptions, text: &str) -> String {
    for (index, ch) in text.char_indices() {
        if ch == '\r' {
            return if text.as_bytes().get(index + 1) == Some(&b'\n') {
                "\r\n".to_string()
            } else {
                "\r".to_string()
            };
        }
        if ch == '\n' {
            return "\n".to_string();
        }
    }
    options.eol.clone()
}
