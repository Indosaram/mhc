//! Port of `claim-receipt/shell-words.ts`: split a shell command line into argv segments while
//! respecting quotes, escapes, separators, substitutions and backticks, and drop heredoc bodies.
//!
//! The heredoc and quoted-delimiter scans are hand-written rather than regex-driven so a quoted
//! terminator (`<<'EOF'`) is recognised without a variable-length look-behind.

const SEPARATORS: [char; 7] = ['\n', ';', '|', '&', '(', ')', '`'];

/// A `#` that starts a word outside quotes begins a comment that runs to the end of the line.
#[must_use]
pub fn without_comment(line: &str) -> String {
    let mut quote: Option<char> = None;
    let mut previous: Option<char> = None;
    for (index, character) in line.char_indices() {
        if let Some(open) = quote {
            if character == open {
                quote = None;
            }
        } else if character == '\'' || character == '"' {
            quote = Some(character);
        } else if character == '#'
            && (index == 0 || previous.is_some_and(char::is_whitespace))
        {
            return line[..index].to_owned();
        }
        previous = Some(character);
    }
    line.to_owned()
}

/// `(?<!<<-?\s*)`: a quote right after a heredoc redirect opens the terminator, not a quoted word.
fn starts_a_heredoc_quote(prefix: &str) -> bool {
    let trimmed = prefix.trim_end();
    trimmed.ends_with("<<") || trimmed.ends_with("<<-")
}

/// The `QUOTED` substitution of upstream's `stripHeredocBodies`: drop quoted spans so a quoted
/// heredoc terminator stays visible to the heredoc scan.
fn without_quoted(line: &str) -> String {
    let characters: Vec<(usize, char)> = line.char_indices().collect();
    let mut kept = String::with_capacity(line.len());
    let mut index = 0usize;
    while index < characters.len() {
        let (at, character) = characters[index];
        if (character == '\'' || character == '"') && !starts_a_heredoc_quote(&line[..at]) {
            let mut cursor = index + 1;
            let mut closed = false;
            while cursor < characters.len() {
                let (_, inner) = characters[cursor];
                if character == '"' && inner == '\\' {
                    cursor += 2;
                    continue;
                }
                if inner == character {
                    closed = true;
                    cursor += 1;
                    break;
                }
                cursor += 1;
            }
            if closed {
                index = cursor;
                continue;
            }
        }
        kept.push(character);
        index += 1;
    }
    kept
}

/// `HEREDOC`: the terminator word of `<<`, `<<-`, `<<'EOF'` or `<<"EOF"`, if the line opens one.
fn heredoc_terminator(line: &str) -> Option<String> {
    let bytes = line.as_bytes();
    let mut index = 0usize;
    while index + 1 < bytes.len() {
        if bytes[index] == b'<' && bytes[index + 1] == b'<' {
            let previous_is_angle = index > 0 && bytes[index - 1] == b'<';
            let next_is_angle = bytes.get(index + 2) == Some(&b'<');
            if !previous_is_angle && !next_is_angle
                && let Some(terminator) = terminator_at(line, index + 2) {
                    return Some(terminator);
                }
        }
        index += 1;
    }
    None
}

fn terminator_at(line: &str, mut cursor: usize) -> Option<String> {
    let bytes = line.as_bytes();
    if bytes.get(cursor) == Some(&b'-') {
        cursor += 1;
    }
    while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
        cursor += 1;
    }
    let quote = match bytes.get(cursor) {
        Some(b'\'') => {
            cursor += 1;
            Some('\'')
        }
        Some(b'"') => {
            cursor += 1;
            Some('"')
        }
        _ => None,
    };
    let start = cursor;
    let is_name_start = bytes
        .get(cursor)
        .is_some_and(|byte| byte.is_ascii_alphabetic() || *byte == b'_');
    if !is_name_start {
        return None;
    }
    while bytes
        .get(cursor)
        .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
    {
        cursor += 1;
    }
    let name = line.get(start..cursor)?.to_owned();
    match quote {
        Some(quote) if !line.get(cursor..).is_some_and(|rest| rest.starts_with(quote)) => None,
        _ => Some(name),
    }
}

/// A heredoc body is file content, not commands: keep the redirect line, drop every line up to the
/// terminator.
#[must_use]
pub fn strip_heredoc_bodies(input: &str) -> String {
    let mut kept: Vec<String> = Vec::new();
    let mut terminator: Option<String> = None;
    for line in input.split('\n') {
        if let Some(current) = terminator.as_ref() {
            if line.trim() == current.as_str() {
                terminator = None;
            }
            continue;
        }
        let stripped = without_comment(line);
        terminator = heredoc_terminator(&without_quoted(&stripped));
        kept.push(stripped);
    }
    kept.join("\n")
}

/// Splits a shell command line into argv segments at `&&`, `||`, `;`, `|`, `&`, newlines, the
/// parentheses of subshells and `$(...)` substitutions, and backticks, honouring single and double
/// quotes so a quoted `&&` or a quoted check name stays inside its argument. Heredoc bodies are
/// dropped.
#[must_use]
pub fn shell_segments(input: &str) -> Vec<Vec<String>> {
    let line = strip_heredoc_bodies(input);
    let characters: Vec<char> = line.chars().collect();
    let mut segments: Vec<Vec<String>> = Vec::new();
    let mut argv: Vec<String> = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut quote: Option<char> = None;
    let mut index = 0usize;
    while index < characters.len() {
        let character = characters[index];
        if let Some(open) = quote {
            if character == open {
                quote = None;
            } else {
                word.push(character);
            }
            index += 1;
            continue;
        }
        if character == '\'' || character == '"' {
            quote = Some(character);
            in_word = true;
            index += 1;
            continue;
        }
        if character == '\\' && index + 1 < characters.len() {
            word.push(characters[index + 1]);
            in_word = true;
            index += 2;
            continue;
        }
        if character == '$' && characters.get(index + 1) == Some(&'(') {
            index += 1;
            continue;
        }
        if SEPARATORS.contains(&character) {
            if in_word {
                argv.push(std::mem::take(&mut word));
            }
            in_word = false;
            if !argv.is_empty() {
                segments.push(std::mem::take(&mut argv));
            }
            index += 1;
            continue;
        }
        if character == ' ' || character == '\t' {
            if in_word {
                argv.push(std::mem::take(&mut word));
            }
            in_word = false;
            index += 1;
            continue;
        }
        word.push(character);
        in_word = true;
        index += 1;
    }
    if in_word {
        argv.push(word);
    }
    if !argv.is_empty() {
        segments.push(argv);
    }
    segments
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoted_separators_stay_inside_their_argument() {
        assert_eq!(
            shell_segments("sh -c 'bun test x && tsgo'"),
            vec![vec!["sh", "-c", "bun test x && tsgo"]]
        );
    }

    #[test]
    fn substitutions_backticks_and_subshells_split_into_segments() {
        assert_eq!(
            shell_segments("for i in 1 2; do out=$(gh pr checks 2); done"),
            vec![
                vec!["for", "i", "in", "1", "2"],
                vec!["do", "out="],
                vec!["gh", "pr", "checks", "2"],
                vec!["done"],
            ]
        );
        assert_eq!(
            shell_segments("x=`bun test x`"),
            vec![vec!["x="], vec!["bun", "test", "x"]]
        );
    }

    #[test]
    fn comments_end_at_a_word_starting_hash_and_here_strings_are_not_heredocs() {
        assert_eq!(without_comment("echo hi # it's a note"), "echo hi ");
        assert_eq!(
            shell_segments("read a <<< foo\nbun test x"),
            vec![
                vec!["read", "a", "<<<", "foo"],
                vec!["bun", "test", "x"]
            ]
        );
        assert_eq!(
            shell_segments("echo \"<<EOF\"\nbun test x"),
            vec![vec!["echo", "<<EOF"], vec!["bun", "test", "x"]]
        );
    }

    #[test]
    fn heredoc_bodies_are_dropped_even_with_a_quoted_terminator_and_a_comment() {
        assert_eq!(
            shell_segments("cat > f <<'EOF' # it's the script\nbun test x\nEOF"),
            vec![vec!["cat", ">", "f", "<<EOF"]]
        );
        assert_eq!(
            heredoc_terminator("cat > f <<'EOF'"),
            Some("EOF".to_owned())
        );
        assert_eq!(heredoc_terminator("read a <<< foo"), None);
    }
}
