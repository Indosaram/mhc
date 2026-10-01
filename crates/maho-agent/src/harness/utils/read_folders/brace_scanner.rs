use super::{header_protection::HeaderProtection, lexical_context::*, lexical_spans::*, types::*};

pub fn scan_braces(source: &str, language: &str, settings: ReadFoldSettings) -> ReadBraceScan {
    let chars: Vec<_> = source.chars().collect();
    let mut ranges = Vec::new();
    let mut stack: Vec<Open> = Vec::new();
    let mut headers = HeaderProtection::default();
    let fail = |reason: &str| ReadBraceScan::ParseFailure {
        reason: reason.into(),
    };
    let mut line = 1;
    let mut i = 0;
    let mut template = false;
    let mut template_depth = 0usize;
    let mut previous = String::new();
    let mut before_word = String::new();
    let mut word_line = 1;
    let mut value_arrow = false;
    let mut expression_end = Some(false);
    let mut import_clause = false;
    let mut ambiguous_angle_depth = None;
    let mut signature_declaration = false;
    if source.starts_with("#!") {
        i = chars.iter().position(|c| *c == '\n').unwrap_or(chars.len());
    }
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied().unwrap_or('\0');
        if c == '\n' {
            line += 1;
        }
        if template {
            if c == '\\' {
                if next == '\n' {
                    line += 1;
                }
                i += 2;
                continue;
            }
            if c == '`' {
                template = false;
                template_depth -= 1;
                expression_end = Some(true);
                previous = "literal".into();
            } else if c == '$' && next == '{' {
                stack.push(Open {
                    char: '{',
                    line,
                    header_line: None,
                    target: false,
                    foldable: false,
                    protected: true,
                    signature: false,
                    interpolation: true,
                    control: false,
                    call: false,
                    value_parameters: false,
                    declaration: false,
                });
                template = false;
                expression_end = Some(false);
                previous = "{".into();
                i += 2;
                continue;
            }
            i += 1;
            continue;
        }
        if c.is_whitespace() || c == '\u{feff}' {
            i += 1;
            continue;
        }
        if c == '/' && next == '/' {
            i = line_comment_end(&chars, i);
            continue;
        }
        if c == '/' && next == '*' {
            let Some(span) = comment_span(&chars, i + 2) else {
                return fail("unterminated_comment");
            };
            let doc = matches!(chars.get(i + 2), Some('*' | '!'));
            if span.newlines + 1 >= settings.min_comment_lines
                && template_depth == 0
                && !stack.iter().any(|o| o.protected)
                && !doc
            {
                ranges.push(ReadLineRange {
                    start_line: line + 1,
                    end_line: line + span.newlines - 1,
                });
            }
            if doc {
                headers.protect(line, line + span.newlines);
            }
            line += span.newlines;
            i = span.end;
            continue;
        }
        if c == '"' || c == '\'' {
            let Some(span) = string_span(&chars, i + 1, c) else {
                return fail("unterminated_string");
            };
            line += span.newlines;
            i = span.end;
            previous = "literal".into();
            expression_end = Some(true);
            if stack.is_empty() {
                import_clause = false;
            }
            continue;
        }
        if c == '`' {
            template = true;
            template_depth += 1;
            i += 1;
            continue;
        }
        if c == '/' {
            let Some(end) = expression_end else {
                return fail("ambiguous_regex_literal");
            };
            if !end {
                let Some(span) = regex_span(&chars, i + 1) else {
                    return fail("ambiguous_or_unterminated_regex");
                };
                i = span.end;
                previous = "literal".into();
                expression_end = Some(true);
                continue;
            }
            i += if next == '=' { 2 } else { 1 };
            expression_end = Some(false);
            previous = "/".into();
            continue;
        }
        if c.is_ascii_alphabetic() || c == '_' || c == '$' {
            let start = i;
            i += 1;
            while chars
                .get(i)
                .is_some_and(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '$')
            {
                i += 1;
            }
            before_word = previous;
            previous = chars[start..i].iter().collect();
            word_line = line;
            if !headers.word(&previous, &before_word, stack.len(), line, language == "ts") {
                return fail("unproved_header");
            }
            expression_end = Some(!EXPRESSION_KEYWORDS.contains(&previous.as_str()));
            if language == "ts" && SIGNATURE_DECLARATIONS.contains(&previous.as_str()) {
                signature_declaration = true;
            }
            if previous == "import" {
                import_clause = true;
            }
            if previous == "from" {
                import_clause = false;
            }
            continue;
        }
        if c.is_ascii_digit() {
            i += 1;
            while chars
                .get(i)
                .is_some_and(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '.')
            {
                i += 1;
            }
            expression_end = Some(true);
            previous = "number".into();
            continue;
        }
        if matches!(c, '{' | '[' | '(') {
            if c == '{' && ambiguous_angle_depth.is_some() {
                return fail("ambiguous_angle_syntax");
            }
            if language != "json"
                && c != '('
                && previous == ","
                && !stack.iter().any(|o| o.protected)
                && !stack.last().is_some_and(|o| o.char == '[' || o.call)
            {
                return fail("ambiguous_binding");
            }
            let class_body = headers.open(c, stack.len(), &previous, line);
            if !headers.punctuation(&c.to_string(), stack.len(), line) {
                return fail("unproved_header");
            }
            let call = c == '(' && is_call_callee(&previous, &before_word);
            let declaration = signature_declaration && !stack.iter().any(|o| o.declaration);
            let signature = declaration
                || headers.active()
                || stack.iter().any(|o| o.signature)
                || (language != "json" && c == '[' && stack.last().is_some_and(|o| o.char == '{'))
                || import_clause
                || ["const", "let", "var", "export", "type", "#", "!"].contains(&previous.as_str())
                || (language != "json" && [":", "<", "&", "|"].contains(&previous.as_str()))
                || (language == "ts" && previous == "=>" && !value_arrow);
            let protected = signature
                || (c == '(' && !call && !(previous == "=>" && value_arrow))
                || stack.iter().any(|o| o.protected);
            let foldable = template_depth == 0
                && c != '('
                && !class_body
                && !protected
                && (c == '{' || expression_end != Some(true) || language == "json");
            stack.push(Open {
                char: c,
                line,
                header_line: Some(if c == '(' { word_line } else { line }),
                target: c != '(' && expression_end != Some(true),
                foldable,
                protected,
                signature,
                interpolation: false,
                control: c == '(' && CONTROLS.contains(&previous.as_str()),
                call,
                value_parameters: c == '('
                    && stack.last().is_some_and(|o| o.call)
                    && ["(", ","].contains(&previous.as_str()),
                declaration,
            });
            value_arrow = false;
            expression_end = Some(false);
            previous = c.to_string();
            i += 1;
            continue;
        }
        if matches!(c, '}' | ']' | ')') {
            let open = stack.pop();
            if ambiguous_angle_depth.is_some_and(|depth| stack.len() < depth) {
                ambiguous_angle_depth = None;
            }
            let Some(open) = open else {
                return fail("unbalanced_delimiters");
            };
            if !matches!((open.char, c), ('{', '}') | ('[', ']') | ('(', ')')) {
                return fail("unbalanced_delimiters");
            }
            if open.interpolation {
                template = true;
                i += 1;
                continue;
            }
            headers.close(&open, stack.len(), line);
            if open.foldable && line.saturating_sub(open.line + 1) >= settings.min_body_lines {
                ranges.push(ReadLineRange {
                    start_line: open.line + 1,
                    end_line: line - 1,
                });
            }
            value_arrow = open.value_parameters;
            if open.declaration {
                signature_declaration = false;
            }
            expression_end = if open.control {
                Some(false)
            } else if c == '}' {
                None
            } else {
                Some(true)
            };
            if c == '}' {
                import_clause = false;
            }
            previous = c.to_string();
            i += 1;
            continue;
        }
        if (c == '+' || c == '-') && next == c {
            previous = format!("{c}{c}");
            i += 2;
            continue;
        }
        if c == '<' && language == "ts" && expression_end == Some(true) {
            if let Some(span) = type_arguments_span(&chars, i + 1) {
                line += span.newlines;
                i = span.end;
                previous = ">".into();
                continue;
            }
            if chars[i + 1..].iter().find(|c| !c.is_whitespace()) != Some(&'{') {
                ambiguous_angle_depth = Some(stack.len());
            }
        }
        let angle_next = if next == '/' {
            chars.get(i + 2).copied().unwrap_or('\0')
        } else {
            next
        };
        if c == '<' && expression_end != Some(true) && angle_next.is_ascii_alphabetic() {
            return fail("ambiguous_angle_syntax");
        }
        if !";:,.?=><!~+-*%&|^".contains(c) {
            return fail("unknown_token");
        }
        if c == ';' {
            import_clause = false;
            signature_declaration = false;
            if ambiguous_angle_depth == Some(stack.len()) {
                ambiguous_angle_depth = None;
            }
        }
        let token = if c == '=' && next == '>' {
            "=>".into()
        } else {
            c.to_string()
        };
        if !headers.punctuation(&token, stack.len(), line) {
            return fail("unproved_header");
        }
        value_arrow = value_arrow && previous == ")" && token == "=>";
        i += if token == "=>" { 2 } else { 1 };
        previous = token;
        expression_end = if c == '.' { None } else { Some(false) };
    }
    if template || template_depth != 0 || !stack.is_empty() || headers.unfinished() {
        fail("unbalanced_or_unproved_header")
    } else {
        ReadBraceScan::Parsed {
            ranges: headers.filter(ranges),
        }
    }
}
