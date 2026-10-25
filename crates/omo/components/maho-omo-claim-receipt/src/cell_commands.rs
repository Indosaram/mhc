//! Port of `claim-receipt/cell-commands.ts`: check kinds run by shell strings inside eval cell
//! code, with the offset of the first such string.
//!
//! Only string literals in command positions (see [`COMMAND_CONTEXT`]) are parsed as command lines,
//! so prints, file contents, asserts, labels and any unknown position never count; lookups are
//! dropped because cells grep and cat for many reasons besides proving existence.

use std::sync::LazyLock;

use regex::Regex;

use crate::invocations::{invoked_kinds, InvokedKind};

/// The check kinds a cell can run: [`InvokedKind`] without the lookup kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CellCheckKind {
    Test,
    Typecheck,
    Lint,
    Build,
    Run,
}

impl CellCheckKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Test => "test",
            Self::Typecheck => "typecheck",
            Self::Lint => "lint",
            Self::Build => "build",
            Self::Run => "run",
        }
    }

    #[must_use]
    pub const fn from_invoked(kind: InvokedKind) -> Option<Self> {
        match kind {
            InvokedKind::Test => Some(Self::Test),
            InvokedKind::Typecheck => Some(Self::Typecheck),
            InvokedKind::Lint => Some(Self::Lint),
            InvokedKind::Build => Some(Self::Build),
            InvokedKind::Run => Some(Self::Run),
            InvokedKind::Lookup => None,
        }
    }
}

/// `cellInvokedKinds`'s result: the deduplicated kinds and the earliest command offset (`-1` when
/// no string literal sits in a command position).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CellInvokedKinds {
    pub kinds: Vec<CellCheckKind>,
    pub offset: i64,
}

static STRING_LITERAL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"\"\"\"[\s\S]*?\"\"\"|'''[\s\S]*?'''|`(?:\\[\s\S]|[^`\\])*`|'(?:\\.|[^'\\\n])*'|"(?:\\.|[^"\\\n])*""#)
        .expect("valid string literal regex")
});
static LIST_OF_STRINGS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"\[\s*(?:\"\"\"|'''|[`'"])[^\[\]]*\]"#).expect("valid list of strings regex")
});
static QUOTES: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"^(?:\"\"\"|'''|[`'"])|(?:\"\"\"|'''|[`'"])$"#).expect("valid quotes regex")
});
static LITERAL_PREFIX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^(?:f|r|rf|fr|b)?$").expect("valid literal prefix regex"));
static INTERPOLATION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\$\{[^}]*\}|\{[A-Za-z_][\w.]*\}").expect("valid interpolation regex")
});

const CONTEXT_WINDOW: usize = 80;

// Positions that carry a command line, the only places a literal counts as one: the first argument
// of a runner call (sh/run/exec/spawn/system/Popen/check_output, and the named helpers s, q, rd,
// sh2, shr, sh_*, run_*, *_run), a runner template tag ($`...`, sh`...`), a command-like field or
// variable (command:, cmds =, script:, job =, s = f"""...), a [name, command] pair nested in a
// command list, and the list of a for loop (`for c in ["bun test x", ...]`).
// Known misses: tuple loops, lists bound to other names and looped later, and argv passed as a
// later argument (`execa("bun", ["test"])`). Everything else is text, so an unknown position fails
// toward "not a command".
const RUNNER: &str = r"(?:sh|sh\d+|shr|shm|sh_\w+|run|run_\w+|\w+_run|exec|execa|execSync|exec_\w+|\w+_exec|spawn|spawnSync|system|popen|Popen|check_output|check_call|call|shell|bash|zsh|ssh|rsh|rd|q|s|\$)";

static COMMAND_CONTEXT: LazyLock<Regex> = LazyLock::new(|| {
    let pattern = [
        format!(r"(?:^|[^\w$]){RUNNER}\s*\(\s*(?:\[\s*)?[A-Za-z]{{0,2}}$"),
        r"(?:^|[^\w])(?:\$|sh|run|exec)\s*$".to_owned(),
        r"\b(?:commands?|cmds?|scripts?|shell|jobs?|steps?|s)\s*[:=]\s*(?:\[\s*)?[A-Za-z]{0,2}$"
            .to_owned(),
        r"\bfor\s*\(?\s*(?:const|let|var)?\s*\w+\s+(?:in|of)\s*$".to_owned(),
    ]
    .join("|");
    Regex::new(&pattern).expect("valid command context regex")
});

/// A [name, command] pair nested in a command list: the inner list right after the outer `[` or a
/// previous `],`.
static NESTED_PAIR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:\[|\],)\s*$").expect("valid nested pair regex"));
static WORD_CHARS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[\w:.-]+$").expect("valid word chars regex"));

fn before(code: &str, at: usize) -> &str {
    let end = at.min(code.len());
    let start = code[..end]
        .char_indices()
        .rev()
        .nth(CONTEXT_WINDOW - 1)
        .map_or(0, |(index, _)| index);
    &code[start..end]
}

fn is_command(code: &str, at: usize) -> bool {
    COMMAND_CONTEXT.is_match(before(code, at))
}

fn is_command_list(code: &str, at: usize) -> bool {
    is_command(code, at) || NESTED_PAIR.is_match(before(code, at))
}

fn unquote(literal: &str) -> String {
    let unquoted = QUOTES.replace_all(literal, "");
    INTERPOLATION.replace_all(&unquoted, "X").into_owned()
}

fn shell_quote(word: &str) -> String {
    format!("'{}'", word.replace('\'', "'\\''"))
}

fn check_kinds(line: &str) -> Vec<CellCheckKind> {
    invoked_kinds(line, 0)
        .into_iter()
        .filter_map(CellCheckKind::from_invoked)
        .collect()
}

// A list such as ["gh", "pr", "checks", n] is one argv (subprocess.run, a Bun spawn call):
// non-literal elements stand in as X.
fn list_words(list: &str) -> Vec<String> {
    let inner = list.get(1..list.len().saturating_sub(1)).unwrap_or("");
    let mut words: Vec<String> = Vec::new();
    for element in inner.split(',') {
        let trimmed = element.trim();
        if trimmed.is_empty() {
            continue;
        }
        let mut word = "X".to_owned();
        if let Some(found) = STRING_LITERAL.find(trimmed) {
            let literal = found.as_str();
            if let Some(prefix) = trimmed.strip_suffix(literal)
                && LITERAL_PREFIX.is_match(prefix) {
                    word = unquote(literal);
                }
        }
        words.push(word);
    }
    words
}

// A list is read as one argv, since a real argv starts with a bare executable. When its first
// element is itself a command line (`for c in ["bun test x", "tsgo"]: sh(c)`) every element is a
// command line, and a [name, command] pair (`["typecheck", "bun run typecheck"]`) contributes its
// command; a message argument inside an argv is never read.
fn list_kinds(list: &str) -> Vec<CellCheckKind> {
    let words = list_words(list);
    let first = words.first().cloned().unwrap_or_default();
    let second = words.get(1).cloned().unwrap_or_default();
    if first.chars().any(char::is_whitespace) {
        return words.iter().flat_map(|word| check_kinds(word)).collect();
    }
    let quoted: Vec<String> = words.iter().map(|word| shell_quote(word)).collect();
    let mut kinds = check_kinds(&quoted.join(" "));
    if words.len() == 2
        && WORD_CHARS.is_match(&first)
        && second.chars().any(char::is_whitespace)
    {
        kinds.extend(check_kinds(&second));
    }
    kinds
}

fn found(kinds: &mut Vec<CellCheckKind>, offset: &mut i64, at: usize, more: &[CellCheckKind]) {
    if more.is_empty() {
        return;
    }
    let at = i64::try_from(at).unwrap_or(i64::MAX);
    if *offset == -1 || at < *offset {
        *offset = at;
    }
    for kind in more {
        if !kinds.contains(kind) {
            kinds.push(*kind);
        }
    }
}

#[must_use]
pub fn cell_invoked_kinds(code: &str) -> CellInvokedKinds {
    let mut kinds: Vec<CellCheckKind> = Vec::new();
    let mut offset: i64 = -1;
    let mut in_lists: Vec<(usize, usize)> = Vec::new();
    for found_list in LIST_OF_STRINGS.find_iter(code) {
        in_lists.push((found_list.start(), found_list.end()));
        if is_command_list(code, found_list.start()) {
            found(
                &mut kinds,
                &mut offset,
                found_list.start(),
                &list_kinds(found_list.as_str()),
            );
        }
    }
    for literal in STRING_LITERAL.find_iter(code) {
        if in_lists
            .iter()
            .any(|(start, end)| literal.start() >= *start && literal.start() < *end)
        {
            continue;
        }
        if is_command(code, literal.start()) {
            found(
                &mut kinds,
                &mut offset,
                literal.start(),
                &check_kinds(&unquote(literal.as_str())),
            );
        }
    }
    CellInvokedKinds { kinds, offset }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(code: &str) -> Vec<&'static str> {
        cell_invoked_kinds(code)
            .kinds
            .into_iter()
            .map(CellCheckKind::as_str)
            .collect()
    }

    #[test]
    fn command_positions_count_and_text_positions_do_not() {
        assert_eq!(kinds("await sh(`bun test x`)"), vec!["test"]);
        assert_eq!(kinds("await tool.bash({ command: 'bun test x' })"), vec!["test"]);
        assert_eq!(kinds("const p = Bun.spawn(['gh', 'pr', 'checks', String(n)])"), vec!["test"]);
        assert_eq!(kinds("await sh(`cd ${WT} && bun test x`)"), vec!["test"]);
        assert_eq!(kinds("print(\"run bun test later\")"), Vec::<&'static str>::new());
        assert_eq!(kinds("await sh(`grep -c vitest log.txt`)"), Vec::<&'static str>::new());
        assert_eq!(
            kinds("log(\"bun test passed\")\nconst label = \"tsc --noEmit\""),
            Vec::<&'static str>::new()
        );
    }

    #[test]
    fn the_earliest_command_position_is_kept() {
        let result = cell_invoked_kinds("await sh(`bun test x`)\nawait sh(`tsgo --noEmit`)");
        assert_eq!(
            result.kinds,
            vec![CellCheckKind::Test, CellCheckKind::Typecheck]
        );
        assert_eq!(result.offset, 9);
        assert_eq!(cell_invoked_kinds("print(1)").offset, -1);
    }
}
