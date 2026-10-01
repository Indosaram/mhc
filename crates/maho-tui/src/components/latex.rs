//! Port of senpi `packages/tui/src/components/latex.ts`.

use std::sync::LazyLock;

use regex::Regex;

fn symbol(command: &str) -> Option<&'static str> {
    Some(match command {
        "\\aleph" => "\u{2135}",
        "\\alpha" => "\u{03b1}",
        "\\approx" => "\u{2248}",
        "\\beta" => "\u{03b2}",
        "\\bowtie" => "\u{22c8}",
        "\\cdot" => "\u{00b7}",
        "\\chi" => "\u{03c7}",
        "\\Delta" => "\u{0394}",
        "\\delta" => "\u{03b4}",
        "\\div" => "\u{00f7}",
        "\\epsilon" => "\u{03f5}",
        "\\equiv" => "\u{2261}",
        "\\eta" => "\u{03b7}",
        "\\exists" => "\u{2203}",
        "\\forall" => "\u{2200}",
        "\\fullouterjoin" => "\u{27d7}",
        "\\Gamma" => "\u{0393}",
        "\\gamma" => "\u{03b3}",
        "\\ge" | "\\geq" => "\u{2265}",
        "\\in" => "\u{2208}",
        "\\infty" => "\u{221e}",
        "\\int" => "\u{222b}",
        "\\iota" => "\u{03b9}",
        "\\Join" => "\u{22c8}",
        "\\kappa" => "\u{03ba}",
        "\\Lambda" => "\u{039b}",
        "\\lambda" => "\u{03bb}",
        "\\le" | "\\leq" => "\u{2264}",
        "\\leftarrow" => "\u{2190}",
        "\\leftouterjoin" => "\u{27d5}",
        "\\leftrightarrow" => "\u{2194}",
        "\\ltimes" => "\u{22c9}",
        "\\mu" => "\u{03bc}",
        "\\nabla" => "\u{2207}",
        "\\ne" | "\\neq" => "\u{2260}",
        "\\ni" => "\u{220b}",
        "\\notin" => "\u{2209}",
        "\\nu" => "\u{03bd}",
        "\\Omega" => "\u{03a9}",
        "\\omega" => "\u{03c9}",
        "\\otimes" => "\u{2297}",
        "\\partial" => "\u{2202}",
        "\\Phi" => "\u{03a6}",
        "\\phi" => "\u{03d5}",
        "\\Pi" => "\u{03a0}",
        "\\pi" => "\u{03c0}",
        "\\pm" => "\u{00b1}",
        "\\prod" => "\u{220f}",
        "\\Psi" => "\u{03a8}",
        "\\psi" => "\u{03c8}",
        "\\rho" => "\u{03c1}",
        "\\rightarrow" | "\\to" => "\u{2192}",
        "\\rightouterjoin" => "\u{27d6}",
        "\\rtimes" => "\u{22ca}",
        "\\Sigma" => "\u{03a3}",
        "\\sigma" => "\u{03c3}",
        "\\sim" => "\u{223c}",
        "\\subset" => "\u{2282}",
        "\\subseteq" => "\u{2286}",
        "\\sum" => "\u{2211}",
        "\\supset" => "\u{2283}",
        "\\supseteq" => "\u{2287}",
        "\\tau" => "\u{03c4}",
        "\\Theta" => "\u{0398}",
        "\\theta" => "\u{03b8}",
        "\\times" => "\u{00d7}",
        "\\Upsilon" => "\u{03a5}",
        "\\upsilon" => "\u{03c5}",
        "\\varepsilon" => "\u{03b5}",
        "\\varphi" => "\u{03c6}",
        "\\vartheta" => "\u{03d1}",
        "\\xi" => "\u{03be}",
        "\\zeta" => "\u{03b6}",
        _ => return None,
    })
}

fn superscript(character: char) -> Option<&'static str> {
    Some(match character {
        '0' => "\u{2070}",
        '1' => "\u{00b9}",
        '2' => "\u{00b2}",
        '3' => "\u{00b3}",
        '4' => "\u{2074}",
        '5' => "\u{2075}",
        '6' => "\u{2076}",
        '7' => "\u{2077}",
        '8' => "\u{2078}",
        '9' => "\u{2079}",
        '+' => "\u{207a}",
        '-' => "\u{207b}",
        '=' => "\u{207c}",
        '(' => "\u{207d}",
        ')' => "\u{207e}",
        'i' => "\u{2071}",
        'n' => "\u{207f}",
        _ => return None,
    })
}

fn subscript(character: char) -> Option<&'static str> {
    Some(match character {
        '0' => "\u{2080}",
        '1' => "\u{2081}",
        '2' => "\u{2082}",
        '3' => "\u{2083}",
        '4' => "\u{2084}",
        '5' => "\u{2085}",
        '6' => "\u{2086}",
        '7' => "\u{2087}",
        '8' => "\u{2088}",
        '9' => "\u{2089}",
        '+' => "\u{208a}",
        '-' => "\u{208b}",
        '=' => "\u{208c}",
        '(' => "\u{208d}",
        ')' => "\u{208e}",
        'a' => "\u{2090}",
        'e' => "\u{2091}",
        'h' => "\u{2095}",
        'i' => "\u{1d62}",
        'j' => "\u{2c7c}",
        'k' => "\u{2096}",
        'l' => "\u{2097}",
        'm' => "\u{2098}",
        'n' => "\u{2099}",
        'o' => "\u{2092}",
        'p' => "\u{209a}",
        'r' => "\u{1d63}",
        's' => "\u{209b}",
        't' => "\u{209c}",
        'u' => "\u{1d64}",
        'v' => "\u{1d65}",
        'x' => "\u{2093}",
        _ => return None,
    })
}

const MAX_FORMULA_LENGTH: usize = 4096;
const MAX_NESTING_DEPTH: usize = 64;

fn is_style_command(command: &str) -> bool {
    matches!(
        command,
        "\\mathrm" | "\\mathbf" | "\\mathit" | "\\text" | "\\operatorname"
    )
}

fn script_text(text: &str, alphabet: fn(char) -> Option<&'static str>) -> Option<String> {
    let mut output = String::new();
    for character in text.chars() {
        output.push_str(alphabet(character)?);
    }
    Some(output)
}

struct LatexParser {
    index: usize,
    input: Vec<char>,
}

impl LatexParser {
    fn new(input: &str) -> Self {
        Self {
            index: 0,
            input: input.chars().collect(),
        }
    }

    fn current(&self) -> Option<char> {
        self.input.get(self.index).copied()
    }

    fn parse(&mut self) -> Option<String> {
        let output = self.parse_sequence(0, false);
        match output {
            Some(output) if self.index == self.input.len() => Some(output),
            _ => None,
        }
    }

    fn parse_sequence(&mut self, depth: usize, stop_at_brace: bool) -> Option<String> {
        if depth > MAX_NESTING_DEPTH {
            return None;
        }
        let mut output = String::new();
        while self.index < self.input.len() {
            let character = self.input[self.index];
            if character == '}' && stop_at_brace {
                return Some(output);
            }
            if character == '\\' {
                let command = self.parse_command(depth)?;
                output.push_str(&command);
                continue;
            }
            if character == '^' || character == '_' {
                let script = self.parse_script(character, depth)?;
                output.push_str(&script);
                continue;
            }
            if character == '{' {
                let group = self.parse_group(depth)?;
                output.push_str(&group);
                continue;
            }
            output.push(character);
            self.index += 1;
        }
        if stop_at_brace {
            None
        } else {
            Some(output)
        }
    }

    fn parse_group(&mut self, depth: usize) -> Option<String> {
        if self.current() != Some('{') {
            return None;
        }
        self.index += 1;
        let body = self.parse_sequence(depth + 1, true)?;
        if self.current() != Some('}') {
            return None;
        }
        self.index += 1;
        Some(body)
    }

    fn parse_command(&mut self, depth: usize) -> Option<String> {
        self.index += 1;
        let Some(first) = self.current() else {
            return Some("\\".to_string());
        };
        if !first.is_ascii_alphabetic() {
            self.index += 1;
            if "_{}[]()$%&#".contains(first) {
                return Some(first.to_string());
            }
            if first == '!' {
                return Some(String::new());
            }
            if ",;:".contains(first) {
                return Some(" ".to_string());
            }
            return Some(format!("\\{first}"));
        }

        let start = self.index;
        while self.current().is_some_and(|c| c.is_ascii_alphabetic()) {
            self.index += 1;
        }
        let command: String = self.input[start..self.index].iter().collect();
        let command = format!("\\{command}");
        if let Some(symbol) = symbol(&command) {
            return Some(symbol.to_string());
        }
        if is_style_command(&command) {
            self.skip_spaces();
            return if self.current() == Some('{') {
                self.parse_group(depth)
            } else {
                Some(command)
            };
        }
        if command == "\\sqrt" {
            self.skip_spaces();
            let body = self.parse_group(depth)?;
            return Some(format!("\u{221a}({body})"));
        }
        if command == "\\frac" {
            self.skip_spaces();
            let numerator = self.parse_group(depth);
            self.skip_spaces();
            let denominator = match numerator {
                Some(_) => self.parse_group(depth),
                None => None,
            };
            return match (numerator, denominator) {
                (Some(numerator), Some(denominator)) => {
                    Some(format!("({numerator})\u{2044}({denominator})"))
                }
                _ => None,
            };
        }
        if command == "\\left" || command == "\\right" {
            self.skip_spaces();
            let delimiter = self.current();
            if delimiter == Some('.') {
                self.index += 1;
                return Some(String::new());
            }
            let escaped_delimiter = if delimiter == Some('\\') {
                self.input.get(self.index + 1).copied()
            } else {
                None
            };
            if let Some(escaped) = escaped_delimiter.filter(|c| "{}".contains(*c)) {
                self.index += 2;
                return Some(escaped.to_string());
            }
            if let Some(delimiter) = delimiter.filter(|c| "()[]{}|".contains(*c)) {
                self.index += 1;
                return Some(delimiter.to_string());
            }
        }
        if command == "\\quad" {
            return Some("  ".to_string());
        }
        let mut fallback = command;
        let argument_start = self.index;
        self.skip_spaces();
        if self.current() != Some('{') {
            self.index = argument_start;
        }
        while self.current() == Some('{') {
            let group = self.parse_group(depth)?;
            fallback.push('{');
            fallback.push_str(&group);
            fallback.push('}');
        }
        Some(fallback)
    }

    fn parse_script(&mut self, marker: char, depth: usize) -> Option<String> {
        self.index += 1;
        let alphabet = if marker == '^' { superscript } else { subscript };
        if self.current() == Some('{') {
            let body = self.parse_group(depth)?;
            return Some(
                script_text(&body, alphabet).unwrap_or_else(|| format!("{marker}{{{body}}}")),
            );
        }
        if self.current() == Some('\\') {
            let body = self.parse_command(depth)?;
            return Some(script_text(&body, alphabet).unwrap_or_else(|| format!("{marker}{body}")));
        }
        let Some(body) = self.current() else {
            return Some(marker.to_string());
        };
        self.index += 1;
        Some(alphabet(body).map(str::to_string).unwrap_or_else(|| format!("{marker}{body}")))
    }

    fn skip_spaces(&mut self) {
        while self.current() == Some(' ') {
            self.index += 1;
        }
    }
}

static JS_WHITESPACE_RUN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"[\t\n\x0B\x0C\r \u{A0}\u{1680}\u{2000}-\u{200A}\u{2028}\u{2029}\u{202F}\u{205F}\u{3000}\u{FEFF}]+",
    )
    .expect("valid regex")
});

static JS_WHITESPACE_EDGE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^[\t\n\x0B\x0C\r \u{A0}\u{1680}\u{2000}-\u{200A}\u{2028}\u{2029}\u{202F}\u{205F}\u{3000}\u{FEFF}]+|[\t\n\x0B\x0C\r \u{A0}\u{1680}\u{2000}-\u{200A}\u{2028}\u{2029}\u{202F}\u{205F}\u{3000}\u{FEFF}]+$",
    )
    .expect("valid regex")
});

static LEADING_COMBINING_MARK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\p{M}").expect("valid regex"));

fn js_trim(value: &str) -> String {
    JS_WHITESPACE_EDGE.replace_all(value, "").into_owned()
}

pub fn latex_to_unicode(formula: &str) -> String {
    let trimmed = js_trim(formula);
    let trimmed = trimmed.as_str();
    let mut rendered = trimmed.to_string();
    if trimmed.encode_utf16().count() <= MAX_FORMULA_LENGTH {
        let normalized = JS_WHITESPACE_RUN.replace_all(trimmed, " ");
        rendered = LatexParser::new(&normalized)
            .parse()
            .unwrap_or_else(|| trimmed.to_string());
    }
    if LEADING_COMBINING_MARK.is_match(&rendered) {
        format!("\u{25cc}{rendered}")
    } else {
        rendered
    }
}

#[cfg(test)]
#[path = "latex_tests.rs"]
mod tests;
