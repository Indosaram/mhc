//! Port of marked v18 `src/helpers.ts` (the subset `components/markdown.ts` reaches).

use std::sync::LazyLock;

use crate::components::markdown_rules as rules;

static INDENT_COMPENSATION: LazyLock<Rule> =
    LazyLock::new(|| Rule::new(rules::OTHER_INDENTCODECOMPENSATION, ""));

pub fn rtrim(value: &str, c: char, invert: bool) -> String {
    let chars: Vec<char> = value.chars().collect();
    let len = chars.len();
    if len == 0 {
        return String::new();
    }
    let mut suffix = 0usize;
    while suffix < len {
        let current = chars[len - suffix - 1];
        let matches = if invert { current != c } else { current == c };
        if matches {
            suffix += 1;
        } else {
            break;
        }
    }
    chars[..len - suffix].iter().collect()
}

pub fn trim_trailing_blank_lines(value: &str) -> String {
    let lines: Vec<&str> = value.split('\n').collect();
    let mut end = lines.len() as i64 - 1;
    while end >= 0 && is_blank_line(lines[end as usize]) {
        end -= 1;
    }
    if lines.len() as i64 - end <= 2 {
        return value.to_string();
    }
    lines[..(end + 1) as usize].join("\n")
}

fn is_blank_line(line: &str) -> bool {
    line.chars().all(|c| c == ' ' || c == '\t')
}

pub fn expand_tabs(line: &str, indent: usize) -> String {
    let mut column = indent;
    let mut expanded = String::new();
    for character in line.chars() {
        if character == '\t' {
            let added = 4 - (column % 4);
            for _ in 0..added {
                expanded.push(' ');
            }
            column += added;
        } else {
            expanded.push(character);
            column += 1;
        }
    }
    expanded
}

pub fn normalize_label(label: &str) -> String {
    label.to_lowercase().to_uppercase().to_lowercase()
}

pub fn find_closing_bracket(value: &str, bracket: &str) -> i64 {
    let open = bracket.as_bytes()[0] as char;
    let close = bracket.as_bytes()[1] as char;
    let chars: Vec<char> = value.chars().collect();
    if !chars.contains(&close) {
        return -1;
    }
    let mut level = 0i64;
    let mut i = 0usize;
    while i < chars.len() {
        if chars[i] == '\\' {
            i += 2;
            continue;
        }
        if chars[i] == open {
            level += 1;
        } else if chars[i] == close {
            level -= 1;
            if level < 0 {
                return i as i64;
            }
        }
        i += 1;
    }
    if level > 0 {
        return -2;
    }
    -1
}

pub fn split_cells(table_row: &str, count: Option<usize>) -> Vec<String> {
    let chars: Vec<char> = table_row.chars().collect();
    let mut row = String::new();
    for (offset, character) in chars.iter().enumerate() {
        if *character != '|' {
            row.push(*character);
            continue;
        }
        let mut escaped = false;
        let mut current = offset as i64 - 1;
        while current >= 0 && chars[current as usize] == '\\' {
            escaped = !escaped;
            current -= 1;
        }
        if escaped {
            row.push('|');
        } else {
            row.push_str(" |");
        }
    }

    let mut cells: Vec<String> = row.split(" |").map(str::to_string).collect();
    if cells.first().is_some_and(|first| first.trim().is_empty()) {
        cells.remove(0);
    }
    if cells.last().is_some_and(|last| last.trim().is_empty()) {
        cells.pop();
    }
    if let Some(count) = count {
        if cells.len() > count {
            cells.truncate(count);
        } else {
            while cells.len() < count {
                cells.push(String::new());
            }
        }
    }
    cells
        .iter()
        .map(|cell| cell.trim().replace("\\|", "|"))
        .collect()
}

pub fn indent_code_compensation(raw: &str, text: &str) -> String {
    let indent_to_code = match INDENT_COMPENSATION.exec(raw) {
        Some(captures) => captures.group_or(1, "").to_string(),
        None => return text.to_string(),
    };
    if indent_to_code.is_empty() {
        return text.to_string();
    }
    text.split('\n')
        .map(|node| {
            let indent_len = node.chars().take_while(|c| c.is_whitespace()).count();
            let keep = indent_len.min(indent_to_code.chars().count());
            node.chars().skip(keep).collect::<String>()
        })
        .collect::<Vec<String>>()
        .join("\n")
}

/// A compiled regex plus the JS flags it was declared with.
pub struct Rule {
    regex: fancy_regex::Regex,
    global: bool,
    case_insensitive: bool,
}

impl Rule {
    pub fn new(source: &str, flags: &str) -> Self {
        let mut pattern = String::new();
        if flags.contains('m') {
            pattern.push_str("(?m)");
        }
        pattern.push_str(source);
        let regex = fancy_regex::RegexBuilder::new(&pattern)
            .case_insensitive(flags.contains('i'))
            .build()
            .unwrap_or_else(|error| panic!("invalid marked rule {source:?}: {error}"));
        Self {
            regex,
            global: flags.contains('g'),
            case_insensitive: flags.contains('i'),
        }
    }

    pub fn is_global(&self) -> bool {
        self.global
    }

    pub fn is_case_insensitive(&self) -> bool {
        self.case_insensitive
    }

    pub fn matches(&self, text: &str) -> bool {
        self.regex.is_match(text).unwrap_or(false)
    }

    pub fn exec<'a>(&self, text: &'a str) -> Option<Captures<'a>> {
        let captures = self.regex.captures(text).ok().flatten()?;
        Captures::new(&captures, text)
    }

    pub fn exec_from<'a>(&self, text: &'a str, last_index: usize) -> Option<Captures<'a>> {
        let captures = self
            .regex
            .captures_from_pos(text, last_index)
            .ok()
            .flatten()?;
        Captures::new(&captures, text)
    }

    pub fn replace_all(&self, text: &str, replacement: &str) -> String {
        self.regex.replace_all(text, replacement).into_owned()
    }

    pub fn exec_all<'a>(&self, text: &'a str) -> Vec<Captures<'a>> {
        let mut out = Vec::new();
        let mut cursor = 0usize;
        loop {
            let Some(captures) = self.exec_from(text, cursor) else {
                break;
            };
            let start = captures.start();
            let end = captures.end();
            let next = if end == start { end + 1 } else { end };
            out.push(captures);
            if next > text.len() {
                break;
            }
            cursor = next;
        }
        out
    }
}

pub struct Captures<'a> {
    groups: Vec<Option<std::ops::Range<usize>>>,
    text: &'a str,
}

impl<'a> Captures<'a> {
    fn new(captures: &fancy_regex::Captures<'_>, text: &'a str) -> Option<Self> {
        let whole = captures.get(0)?;
        if whole.start() != 0 {
            return None;
        }
        let mut groups = Vec::with_capacity(captures.len());
        for index in 0..captures.len() {
            groups.push(captures.get(index).map(|m| m.start()..m.end()));
        }
        Some(Self { groups, text })
    }

    fn range(&self, index: usize) -> Option<std::ops::Range<usize>> {
        self.groups.get(index).cloned().flatten()
    }

    pub fn whole(&self) -> &'a str {
        self.range(0).map(|r| &self.text[r]).unwrap_or_default()
    }

    pub fn start(&self) -> usize {
        self.range(0).map(|r| r.start).unwrap_or(0)
    }

    pub fn end(&self) -> usize {
        self.range(0).map(|r| r.end).unwrap_or(0)
    }

    pub fn text(&self) -> &'a str {
        self.text
    }

    pub fn group(&self, index: usize) -> Option<&'a str> {
        self.range(index).map(|r| &self.text[r])
    }

    pub fn group_or(&self, index: usize, fallback: &'a str) -> &'a str {
        self.group(index).unwrap_or(fallback)
    }

    pub fn group_start(&self, index: usize) -> Option<usize> {
        self.range(index).map(|r| r.start)
    }

    pub fn len(&self) -> usize {
        self.groups.len()
    }
}
