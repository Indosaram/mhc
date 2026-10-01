#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub end: usize,
    pub newlines: usize,
}
pub fn string_span(source: &[char], start: usize, delimiter: char) -> Option<Span> {
    let mut newlines = 0;
    let mut i = start;
    while i < source.len() {
        if source[i] == delimiter {
            return Some(Span {
                end: i + 1,
                newlines,
            });
        }
        if matches!(source[i], '\n' | '\r') {
            return None;
        }
        if source[i] == '\\' {
            if source.get(i + 1) == Some(&'\r') && source.get(i + 2) == Some(&'\n') {
                i += 1;
            }
            if source.get(i + 1) == Some(&'\n') {
                newlines += 1;
            }
            i += 1;
        }
        i += 1;
    }
    None
}
pub fn type_arguments_span(source: &[char], start: usize) -> Option<Span> {
    let mut delimiters = vec!['<'];
    let mut newlines = 0;
    let mut i = start;
    while i < source.len() {
        let c = source[i];
        if c == '\n' {
            newlines += 1;
        }
        if c == '"' || c == '\'' {
            let span = string_span(source, i + 1, c)?;
            newlines += span.newlines;
            i = span.end - 1;
        } else if c == '<' || c == '[' {
            delimiters.push(c);
        } else if c == '>' || c == ']' {
            if delimiters.pop() != Some(if c == '>' { '<' } else { '[' }) {
                return None;
            }
            if delimiters.is_empty() {
                return Some(Span {
                    end: i + 1,
                    newlines,
                });
            }
        } else if !(c.is_ascii_alphanumeric() || c.is_whitespace() || "_$,.?|&".contains(c)) {
            return None;
        }
        i += 1;
    }
    None
}
pub fn line_comment_end(source: &[char], start: usize) -> usize {
    let mut end = start;
    while end < source.len() && !matches!(source[end], '\n' | '\r' | '\u{2028}' | '\u{2029}') {
        end += 1;
    }
    end
}
pub fn comment_span(source: &[char], start: usize) -> Option<Span> {
    let mut newlines = 0;
    for i in start..source.len() {
        if source[i] == '\n' {
            newlines += 1;
        }
        if source[i] == '*' && source.get(i + 1) == Some(&'/') {
            return Some(Span {
                end: i + 2,
                newlines,
            });
        }
    }
    None
}
pub fn regex_span(source: &[char], start: usize) -> Option<Span> {
    let mut character_class = false;
    let mut i = start;
    while i < source.len() {
        if matches!(source[i], '\n' | '\r' | '\u{2028}' | '\u{2029}') {
            return None;
        }
        if source[i] == '\\' {
            if source
                .get(i + 1)
                .is_some_and(|c| matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}'))
            {
                return None;
            }
            i += 2;
            continue;
        }
        if source[i] == '[' {
            character_class = true;
        }
        if source[i] == ']' {
            character_class = false;
        }
        if source[i] == '/' && !character_class {
            let mut end = i + 1;
            while source.get(end).is_some_and(char::is_ascii_alphabetic) {
                end += 1;
            }
            let flags = &source[i + 1..end];
            if flags
                .iter()
                .enumerate()
                .any(|(n, c)| !"dgimsuy".contains(*c) || flags[..n].contains(c))
            {
                return None;
            }
            return Some(Span { end, newlines: 0 });
        }
        i += 1;
    }
    None
}
