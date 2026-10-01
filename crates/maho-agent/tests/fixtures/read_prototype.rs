#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fold {
    pub start: usize,
    pub end: usize,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prototype {
    pub text: String,
    pub folds: Vec<Fold>,
    pub reason: String,
    pub fallback_reason: Option<String>,
}
fn string(
    source: &[char],
    mut i: usize,
    delimiter: &[char],
    multiline: bool,
    raw: bool,
) -> Option<(usize, usize)> {
    let mut newlines = 0;
    while i < source.len() {
        if source[i..].starts_with(delimiter) {
            return Some((i + delimiter.len(), newlines));
        }
        if source[i] == '\n' {
            if !multiline {
                return None;
            }
            newlines += 1;
        }
        if source[i] == '\\' && !raw {
            if source.get(i + 1) == Some(&'\n') {
                newlines += 1;
            }
            i += 1;
        }
        i += 1;
    }
    None
}
fn braces(source: &str, language: &str) -> Result<Vec<Fold>, &'static str> {
    let s: Vec<_> = source.chars().collect();
    let mut stack: Vec<(char, usize, bool, bool, bool)> = Vec::new();
    let mut ranges = Vec::new();
    let mut line = 1;
    let mut i = 0;
    let mut template = false;
    let mut depth = 0usize;
    let mut previous = String::new();
    let mut end = Some(false);
    let mut import = false;
    if source.starts_with("#!") && !source.starts_with("#![") {
        i = s.iter().position(|c| *c == '\n').unwrap_or(s.len());
    }
    while i < s.len() {
        let c = s[i];
        let next = s.get(i + 1).copied().unwrap_or('\0');
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
                depth -= 1;
                end = Some(true);
                previous = "literal".into();
            } else if c == '$' && next == '{' {
                stack.push(('{', line, false, true, false));
                template = false;
                end = Some(false);
                previous = "{".into();
                i += 2;
                continue;
            }
            i += 1;
            continue;
        }
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if c == '/' && next == '/' {
            while i < s.len() && s[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if c == '/' && next == '*' {
            let mut nesting = 1;
            let mut j = i + 2;
            let mut nl = 0;
            while j < s.len() {
                if s[j] == '\n' {
                    nl += 1;
                }
                if language == "rust" && s[j..].starts_with(&['/', '*']) {
                    nesting += 1;
                    j += 2;
                    continue;
                }
                if s[j..].starts_with(&['*', '/']) {
                    nesting -= 1;
                    j += 2;
                    if nesting == 0 {
                        break;
                    }
                    continue;
                }
                j += 1;
            }
            if nesting != 0 {
                return Err("unterminated_comment");
            }
            if nl >= 5 && depth == 0 && !matches!(s.get(i + 2), Some('*' | '!')) {
                ranges.push(Fold {
                    start: line + 1,
                    end: line + nl - 1,
                });
            }
            line += nl;
            i = j;
            continue;
        }
        if language == "rust" && (c == 'r' || c == 'b' && next == 'r') {
            let mut j = i + if c == 'b' { 2 } else { 1 };
            let start = j;
            while s.get(j) == Some(&'#') {
                j += 1;
            }
            if s.get(j) == Some(&'"') {
                let mut delimiter = vec!['"'];
                delimiter.extend_from_slice(&s[start..j]);
                let (stop, nl) =
                    string(&s, j + 1, &delimiter, true, true).ok_or("unterminated_raw_string")?;
                line += nl;
                i = stop;
                previous = "literal".into();
                end = Some(true);
                continue;
            }
        }
        if language == "rust"
            && c == '\''
            && s.get(i + 1)
                .is_some_and(|c| c.is_ascii_alphabetic() || *c == '_')
        {
            let mut j = i + 2;
            while s
                .get(j)
                .is_some_and(|c| c.is_ascii_alphanumeric() || *c == '_')
            {
                j += 1;
            }
            if s.get(j) != Some(&'\'') {
                if !["&", "<", ",", ":", "+", "break", "continue"].contains(&previous.as_str())
                    && s.get(j) != Some(&':')
                {
                    return Err("unterminated_char_literal");
                }
                i = j;
                previous = "lifetime".into();
                end = Some(true);
                continue;
            }
        }
        if c == '"' || c == '\'' {
            let (stop, nl) = string(&s, i + 1, &[c], language == "rust" && c == '"', false)
                .ok_or("unterminated_string")?;
            line += nl;
            i = stop;
            previous = "literal".into();
            end = Some(true);
            if stack.is_empty() {
                import = false;
            }
            continue;
        }
        if c == '`' {
            if language == "rust" || language == "json" {
                return Err("unexpected_backtick");
            }
            template = true;
            depth += 1;
            i += 1;
            continue;
        }
        if c == '/' {
            if language != "rust" {
                let expression = end.ok_or("ambiguous_regex_literal")?;
                if !expression {
                    let mut j = i + 1;
                    let mut class = false;
                    let mut closed = false;
                    while j < s.len() {
                        if matches!(s[j], '\n' | '\r') {
                            break;
                        }
                        if s[j] == '\\' {
                            j += 2;
                            continue;
                        }
                        if s[j] == '[' {
                            class = true;
                        }
                        if s[j] == ']' {
                            class = false;
                        }
                        if s[j] == '/' && !class {
                            j += 1;
                            while s.get(j).is_some_and(char::is_ascii_alphabetic) {
                                j += 1;
                            }
                            closed = true;
                            break;
                        }
                        j += 1;
                    }
                    if !closed {
                        return Err("unterminated_regex_literal");
                    }
                    i = j;
                    previous = "literal".into();
                    end = Some(true);
                    continue;
                }
            }
            i += if next == '=' { 2 } else { 1 };
            end = Some(false);
            previous = "/".into();
            continue;
        }
        if c.is_ascii_alphabetic() || "_$".contains(c) {
            let start = i;
            i += 1;
            while s
                .get(i)
                .is_some_and(|c| c.is_ascii_alphanumeric() || "_$".contains(*c))
            {
                i += 1;
            }
            previous = s[start..i].iter().collect();
            end = Some(
                ![
                    "return",
                    "throw",
                    "yield",
                    "await",
                    "case",
                    "typeof",
                    "void",
                    "delete",
                    "in",
                    "of",
                    "instanceof",
                ]
                .contains(&previous.as_str()),
            );
            if previous == "import" {
                import = true;
            }
            if previous == "from" {
                import = false;
            }
            continue;
        }
        if c.is_ascii_digit() {
            i += 1;
            while s
                .get(i)
                .is_some_and(|c| c.is_ascii_alphanumeric() || "_.".contains(*c))
            {
                i += 1;
            }
            end = Some(true);
            previous = "number".into();
            continue;
        }
        if "{[(".contains(c) {
            let fold = depth == 0
                && c != '('
                && !import
                && !["export", "type", "#", "!"].contains(&previous.as_str())
                && (c == '{' || end != Some(true) || language == "json");
            stack.push((
                c,
                line,
                fold,
                false,
                c == '('
                    && ["if", "while", "for", "switch", "catch", "with"]
                        .contains(&previous.as_str()),
            ));
            end = Some(false);
            previous = c.to_string();
            i += 1;
            continue;
        }
        if "}])".contains(c) {
            let open = stack.pop().ok_or("unbalanced_delimiters")?;
            if !matches!((open.0, c), ('{', '}') | ('[', ']') | ('(', ')')) {
                return Err("unbalanced_delimiters");
            }
            if open.3 {
                template = true;
                i += 1;
                continue;
            }
            if open.2 && line.saturating_sub(open.1 + 1) >= 4 {
                ranges.push(Fold {
                    start: open.1 + 1,
                    end: line - 1,
                });
            }
            end = if open.4 {
                Some(false)
            } else if c == '}' {
                None
            } else {
                Some(true)
            };
            if c == '}' {
                import = false;
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
        if c == ';' {
            import = false;
        }
        previous = if c == '=' && next == '>' {
            "=>".into()
        } else {
            c.to_string()
        };
        i += if previous == "=>" { 2 } else { 1 };
        end = if c == '.' { None } else { Some(false) };
    }
    if template || depth != 0 || !stack.is_empty() {
        Err("unbalanced_delimiters")
    } else {
        Ok(ranges)
    }
}
struct Statement {
    first: usize,
    last: usize,
    indent: usize,
    code: String,
}
fn python(source: &str) -> Result<Vec<Fold>, &'static str> {
    let s: Vec<_> = source.chars().collect();
    let mut statements: Vec<Statement> = Vec::new();
    let mut brackets = Vec::new();
    let mut levels = vec![0];
    let (mut i, mut line, mut first, mut indent, mut column) = (0, 1, 1, 0, 0);
    let mut code = String::new();
    let mut line_start = true;
    let mut indentation_error = false;
    let flush = |code: &mut String,
                 line,
                 first,
                 indent,
                 statements: &mut Vec<Statement>,
                 levels: &mut Vec<usize>,
                 error: &mut bool| {
        if code.trim().is_empty() {
            code.clear();
            return;
        }
        let opens = statements
            .last()
            .is_some_and(|s| s.code.trim_end().ends_with(':'));
        if indent > *levels.last().expect("fixture invariant") {
            if !opens {
                *error = true;
            }
            levels.push(indent);
        } else {
            if opens {
                *error = true;
            }
            while levels.len() > 1 && indent < *levels.last().expect("fixture invariant") {
                levels.pop();
            }
            if indent != *levels.last().expect("fixture invariant") {
                *error = true;
            }
        }
        statements.push(Statement {
            first,
            last: line,
            indent,
            code: code.trim().into(),
        });
        code.clear();
    };
    while i < s.len() {
        let c = s[i];
        if c == '\n' {
            if brackets.is_empty() {
                flush(
                    &mut code,
                    line,
                    first,
                    indent,
                    &mut statements,
                    &mut levels,
                    &mut indentation_error,
                );
            } else {
                code.push(' ');
            }
            line += 1;
            column = 0;
            line_start = true;
            i += 1;
            continue;
        }
        if c == '\r' {
            i += 1;
            continue;
        }
        if line_start && (c == ' ' || c == '\t') {
            if c == '\t' {
                return Err("python_mixed_indentation");
            }
            column += 1;
            i += 1;
            continue;
        }
        if c == '#' {
            while i < s.len() && s[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if code.trim().is_empty() && c != ' ' {
            first = line;
            indent = column;
        }
        line_start = false;
        if c == '\\' {
            if s.get(i + 1) != Some(&'\n') {
                return Err("python_invalid_continuation");
            }
            line += 1;
            column = 0;
            line_start = true;
            code.push(' ');
            i += 2;
            continue;
        }
        if c == '"' || c == '\'' {
            let triple = s[i..].starts_with(&[c, c, c]);
            let delimiter = if triple { vec![c, c, c] } else { vec![c] };
            let (stop, nl) = string(&s, i + delimiter.len(), &delimiter, triple, false)
                .ok_or("python_unterminated_string")?;
            code.push_str(" STRING ");
            line += nl;
            i = stop;
            continue;
        }
        if "([{ ".trim_end().contains(c) {
            brackets.push(c);
        }
        if ")] }".replace(' ', "").contains(c) {
            let open = brackets.pop().ok_or("python_unbalanced_delimiters")?;
            if !matches!((open, c), ('(', ')') | ('[', ']') | ('{', '}')) {
                return Err("python_unbalanced_delimiters");
            }
        }
        code.push(c);
        i += 1;
    }
    flush(
        &mut code,
        line,
        first,
        indent,
        &mut statements,
        &mut levels,
        &mut indentation_error,
    );
    if !brackets.is_empty() {
        return Err("python_unbalanced_delimiters");
    }
    if indentation_error || statements.last().is_some_and(|s| s.code.ends_with(':')) {
        return Err("python_invalid_indentation");
    }
    let mut ranges = Vec::new();
    for (n, header) in statements.iter().enumerate() {
        let code = header.code.strip_prefix("async ").unwrap_or(&header.code);
        if header.first != header.last
            || !code.starts_with("def ")
            || !code.contains('(')
            || !code.contains(')')
            || !code.ends_with(':')
        {
            continue;
        }
        let mut next = n + 1;
        while next < statements.len() && statements[next].indent > header.indent {
            next += 1;
        }
        if next == n + 1 {
            return Err("python_empty_suite");
        }
        let end = statements[next - 1].last - 1;
        if end - header.first >= 4 {
            ranges.push(Fold {
                start: header.first + 1,
                end,
            });
        }
    }
    Ok(ranges)
}
pub fn heuristic(source: &str, language: &str) -> Prototype {
    let raw = |reason: &str| Prototype {
        text: source.into(),
        folds: vec![],
        reason: reason.into(),
        fallback_reason: Some(reason.into()),
    };
    let lines: Vec<_> = source.split('\n').collect();
    if language == "markdown" || language == "txt" {
        return raw("prose_exempt");
    }
    if !(100..=2000).contains(&lines.len()) || source.len() > 51200 {
        return raw("size_gate");
    }
    if language == "json" && serde_json::from_str::<serde_json::Value>(source).is_err() {
        return raw("parse_failure");
    }
    let mut ordered = match if language == "python" {
        python(source)
    } else {
        braces(source, language)
    } {
        Ok(ranges) => ranges,
        Err(reason) => return raw(reason),
    };
    ordered.sort_by_key(|r| (r.start, std::cmp::Reverse(r.end)));
    let mut selected: Vec<_> = ordered
        .iter()
        .enumerate()
        .filter(|(i, r)| {
            !ordered[..*i]
                .iter()
                .any(|p| p.start <= r.start && p.end >= r.end)
        })
        .map(|(_, r)| r.clone())
        .collect();
    let visible =
        |folds: &[Fold]| lines.len() - folds.iter().map(|f| f.end - f.start + 1).sum::<usize>();
    while visible(&selected) < 50 {
        let mut refined = false;
        for parent in &selected {
            let children: Vec<_> = ordered
                .iter()
                .filter(|r| r.start > parent.start && r.end < parent.end)
                .collect();
            let direct = children
                .iter()
                .enumerate()
                .filter(|(i, r)| {
                    !children[..*i]
                        .iter()
                        .any(|p| p.start <= r.start && p.end >= r.end)
                })
                .map(|(_, r)| (*r).clone());
            let mut next: Vec<_> = selected
                .iter()
                .filter(|r| *r != parent)
                .cloned()
                .chain(direct)
                .collect();
            next.sort_by_key(|r| r.start);
            if visible(&next) <= 100 {
                selected = next;
                refined = true;
                break;
            }
        }
        if !refined {
            return raw("visible_budget_unreachable");
        }
    }
    if selected.is_empty() || visible(&selected) > 100 {
        return raw("skeleton_exceeds_budget");
    }
    let mut parts = Vec::new();
    let mut cursor = 0;
    for fold in &selected {
        parts.extend(lines[cursor..fold.start - 1].iter().map(|s| s.to_string()));
        parts.push("…".into());
        cursor = fold.end;
    }
    parts.extend(lines[cursor..].iter().map(|s| s.to_string()));
    parts.push(String::new());
    parts.push(format!(
        "[Elided source: {}]",
        selected
            .iter()
            .map(|f| format!("offset={} limit={}", f.start, f.end - f.start + 1))
            .collect::<Vec<_>>()
            .join("; ")
    ));
    let text = parts.join("\n");
    if text.encode_utf16().count() >= source.encode_utf16().count() {
        return raw("no_byte_saving");
    }
    Prototype {
        text,
        folds: selected,
        reason: "folded".into(),
        fallback_reason: None,
    }
}
pub fn retained_exact(source: &str, candidate: &Prototype) -> bool {
    if candidate.folds.is_empty() {
        return source == candidate.text;
    }
    let lines: Vec<_> = source.split('\n').collect();
    let output: Vec<_> = candidate.text.split('\n').collect();
    let mut out = 0;
    let mut line = 1;
    while line <= lines.len() {
        if let Some(fold) = candidate.folds.iter().find(|f| f.start == line) {
            if output.get(out) != Some(&"…") {
                return false;
            }
            line = fold.end;
        } else if output.get(out) != Some(&lines[line - 1]) {
            return false;
        }
        out += 1;
        line += 1;
    }
    candidate.folds.iter().all(|f| {
        output[out..].join("\n").contains(&format!(
            "offset={} limit={}",
            f.start,
            f.end - f.start + 1
        ))
    })
}
