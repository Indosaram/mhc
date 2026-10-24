//! One Windows command line from an argv, for `CreateProcessW` (senpi#3566, plan todo 4: the breakaway shim). Windows
//! passes a single string; the program's C runtime splits it back with the MSVC rules, so each argument is quoted such
//! that the split returns it unchanged: spaces, tabs and quotes force quoting, a quote is escaped as `\"`, and
//! backslashes are doubled only when they precede a quote (inside the argument or before the closing quote).
//!
//! senpi `modes/rpc/tool-child/windows-command-line.ts`.

/// JS `\s` as the upstream regex `/[\s"]/` sees it: Unicode White_Space plus U+FEFF.
fn is_js_whitespace(character: char) -> bool {
    character.is_whitespace() || character == '\u{feff}'
}

pub fn quote_windows_argument(argument: &str) -> String {
    if !argument.is_empty() && !argument.chars().any(|c| is_js_whitespace(c) || c == '"') {
        return argument.to_string();
    }
    let mut quoted = String::from("\"");
    let mut backslashes = 0usize;
    for character in argument.chars() {
        if character == '\\' {
            backslashes += 1;
            continue;
        }
        if character == '"' {
            quoted.push_str(&"\\".repeat(backslashes * 2 + 1));
            quoted.push('"');
        } else {
            quoted.push_str(&"\\".repeat(backslashes));
            quoted.push(character);
        }
        backslashes = 0;
    }
    quoted.push_str(&"\\".repeat(backslashes * 2));
    quoted.push('"');
    quoted
}

pub fn windows_command_line<S: AsRef<str>>(argv: &[S]) -> String {
    argv.iter()
        .map(|argument| quote_windows_argument(argument.as_ref()))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// MSVC argv splitting (the C runtime's rules for a program's own command line, past argv[0]).
    fn split_msvc(command_line: &str) -> Vec<String> {
        let chars: Vec<char> = command_line.chars().collect();
        let mut args = Vec::new();
        let mut current = String::new();
        let mut in_quotes = false;
        let mut started = false;
        let mut index = 0;
        while index < chars.len() {
            let character = chars[index];
            if character == '\\' {
                let mut run = 0;
                while chars.get(index) == Some(&'\\') {
                    run += 1;
                    index += 1;
                }
                if chars.get(index) == Some(&'"') {
                    current.push_str(&"\\".repeat(run / 2));
                    if run % 2 == 1 {
                        current.push('"');
                        index += 1;
                    }
                } else {
                    current.push_str(&"\\".repeat(run));
                }
                started = true;
                continue;
            }
            if character == '"' {
                if in_quotes && chars.get(index + 1) == Some(&'"') {
                    current.push('"');
                    index += 2;
                    continue;
                }
                in_quotes = !in_quotes;
                started = true;
                index += 1;
                continue;
            }
            if !in_quotes && (character == ' ' || character == '\t') {
                if started {
                    args.push(std::mem::take(&mut current));
                }
                current.clear();
                started = false;
                index += 1;
                continue;
            }
            current.push(character);
            started = true;
            index += 1;
        }
        if started {
            args.push(current);
        }
        args
    }

    const CASES: [&str; 13] = [
        "plain",
        "",
        "with space",
        "tab\there",
        "a\"quote",
        "\"leading and trailing\"",
        "trailing\\",
        "trailing space \\",
        "C:\\Program Files\\x\\",
        "back\\\"slash-quote",
        "\\\\\"double",
        "\\\\server\\share",
        "unicode é ✓",
    ];

    #[test]
    fn round_trips_every_argument_through_the_msvc_splitting_rules() {
        assert_eq!(split_msvc(&windows_command_line(&CASES)), CASES.to_vec());
    }

    #[test]
    fn leaves_simple_arguments_unquoted_and_quotes_the_empty_argument() {
        assert_eq!(quote_windows_argument("plain"), "plain");
        assert_eq!(
            quote_windows_argument("C:\\tools\\x.exe"),
            "C:\\tools\\x.exe"
        );
        assert_eq!(quote_windows_argument(""), "\"\"");
    }

    #[test]
    fn doubles_backslashes_only_before_a_quote() {
        assert_eq!(
            quote_windows_argument("dir with space\\"),
            "\"dir with space\\\\\""
        );
        assert_eq!(quote_windows_argument("a\\\"b"), "\"a\\\\\\\"b\"");
    }
}
