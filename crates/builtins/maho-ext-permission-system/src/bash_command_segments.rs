//! Port of `permission-system/bash-command-segments.ts` (senpi v2026.10.27-2, senpi#4309).
//!
//! Splits a shell command into the simple commands it would run, so the `bash` permission decision
//! can judge every one of them, not only the first.
//!
//! The lexer follows bash's word and operator rules closely enough to find each simple command:
//! `;`, `&&`, `||`, `|`, `|&`, `&` and line breaks end one; subshells `( … )`, command substitution
//! `$( … )` and backticks, process substitution `<( … )` / `>( … )`, `${ … }` and `$(( … ))` and the
//! bodies of unquoted here-documents are searched for the commands they run; redirections and their
//! targets are dropped. Every command name is then normalized: leading assignments and reserved
//! words are skipped, a path is reduced to its basename, and a wrapper (`env`, `sudo`, `xargs`,
//! `bash -c`, `find -exec`, …) yields both its own name and the command it runs.
//!
//! Anything the lexer does not understand (an unterminated quote, a `case`, a function definition,
//! a command name built by an expansion, an unknown wrapper option) makes the whole command
//! `unparsed`, and the caller must not let any command-specific rule allow it.
//!
//! The TS module throws `Unparsed` and keeps the step counter in module state; here every scan
//! returns `Result<_, Unparsed>` and the counter lives in the per-call [`State`]. Indices are over
//! `char`s instead of UTF-16 code units; every delimiter the lexer inspects is ASCII.

use std::cell::Cell;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BashCommandAnalysis {
    Parsed { segments: Vec<Vec<String>> },
    Unparsed,
}

#[derive(Clone, Debug, Default)]
struct Word {
    text: String,
    /// Part of the word comes from an expansion or a glob, so its runtime value is unknown.
    dynamic: bool,
    /// Part of the word was quoted or escaped, so it is never a reserved word or an operator.
    quoted: bool,
    /// Length (in chars) of the leading text that is plain: only it can name an assignment.
    plain_prefix: usize,
    plain_ended: bool,
    /// An unquoted `{` that brace expansion may turn into several words.
    brace: bool,
    /// An unquoted `*`, `?` or `[` that pathname expansion may replace.
    glob: bool,
}

#[derive(Debug)]
struct Unparsed;
type Lex<T> = Result<T, Unparsed>;

const MAX_DEPTH: usize = 12;
/// Longer commands are not lexed at all: they are `unparsed`, so only a `*` rule can decide them.
pub const MAX_COMMAND_LENGTH: usize = 64 * 1024;
/// Total characters the lexer may visit across every nested scan of one command before giving up.
const MAX_STEPS: usize = 4_000_000;
/// Commands a command may nest through wrappers (`env`, `nice`, `timeout`, `xargs`, `find -exec`)
/// and inline scripts (`eval`, `sh -c`, `su -c`, `trap`). Upstream recurses until the JS stack
/// overflows and catches the RangeError as `unparsed`; Rust cannot catch a stack overflow, so the
/// nesting is bounded explicitly and a deeper chain is `unparsed` too.
const MAX_WRAPPER_DEPTH: usize = 64;
/// Words a best-effort candidate needs: the longest `BashArity` prefix is three words.
const CANDIDATE_WORDS: usize = 3;

thread_local! {
    static LAST_STEPS: Cell<usize> = const { Cell::new(0) };
}

/// Lexer work shared by every nested scan of one command.
struct State {
    steps: usize,
    /// Current nesting of `expand_simple_command`.
    wrapper_depth: usize,
}
impl State {
    /// Counts lexer work; overrunning the budget makes the command `unparsed`, never slow.
    fn step(&mut self) -> Lex<()> {
        self.steps += 1;
        if self.steps > MAX_STEPS {
            Err(Unparsed)
        } else {
            Ok(())
        }
    }
}

fn is_name_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_'
}
fn is_digits(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit())
}

/// Length of a leading `NAME=`, `NAME[...]=` or `NAME+=` (the TS `ASSIGNMENT` regex), in chars.
fn assignment_len(text: &str) -> Option<usize> {
    let chars: Vec<char> = text.chars().collect();
    let first = *chars.first()?;
    if !(first.is_ascii_alphabetic() || first == '_') {
        return None;
    }
    let mut index = 1;
    while index < chars.len() && is_name_char(chars[index]) {
        index += 1;
    }
    if chars.get(index) == Some(&'[') {
        let close = chars[index + 1..].iter().position(|ch| *ch == ']')?;
        index += close + 2;
    }
    if chars.get(index) == Some(&'+') {
        index += 1;
    }
    (chars.get(index) == Some(&'=')).then_some(index + 1)
}

/// A leading `NAME=value` word, decided on its plain prefix as bash decides it before expansion.
fn is_assignment(word: &Word) -> bool {
    assignment_len(&word.text).is_some_and(|len| len <= word.plain_prefix)
}

fn find_char(source: &[char], from: usize, needle: char) -> Option<usize> {
    source
        .get(from..)?
        .iter()
        .position(|ch| *ch == needle)
        .map(|offset| from + offset)
}

/// Index just past the `)` that closes a `(` opened right before `start`, skipping quoted text.
fn match_paren(state: &mut State, source: &[char], start: usize) -> Lex<usize> {
    let mut depth = 1;
    let mut index = start;
    while index < source.len() {
        state.step()?;
        match source[index] {
            '\\' => {
                index += 2;
                continue;
            }
            '\'' => {
                index = find_char(source, index + 1, '\'').ok_or(Unparsed)? + 1;
                continue;
            }
            '"' => {
                index = skip_double_quoted(state, source, index + 1)?;
                continue;
            }
            '`' => {
                index = skip_backticks(state, source, index + 1)?;
                continue;
            }
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Ok(index + 1);
                }
            }
            _ => {}
        }
        index += 1;
    }
    Err(Unparsed)
}

/// Index just past the closing `"` of a double-quoted string that starts at `start`.
fn skip_double_quoted(state: &mut State, source: &[char], start: usize) -> Lex<usize> {
    let mut index = start;
    while index < source.len() {
        state.step()?;
        match source[index] {
            '\\' => {
                index += 2;
                continue;
            }
            '"' => return Ok(index + 1),
            '`' => {
                index = skip_backticks(state, source, index + 1)?;
                continue;
            }
            '$' if source.get(index + 1) == Some(&'(') => {
                index = match_paren(state, source, index + 2)?;
                continue;
            }
            _ => {}
        }
        index += 1;
    }
    Err(Unparsed)
}

/// Index just past the closing backtick of a backtick substitution that starts at `start`.
fn skip_backticks(state: &mut State, source: &[char], start: usize) -> Lex<usize> {
    let mut index = start;
    while index < source.len() {
        state.step()?;
        match source[index] {
            '\\' => {
                index += 2;
                continue;
            }
            '`' => return Ok(index + 1),
            _ => {}
        }
        index += 1;
    }
    Err(Unparsed)
}

/// Index just past the `}` closing a `${` whose body starts at `start`.
fn match_brace(state: &mut State, source: &[char], start: usize) -> Lex<usize> {
    let mut depth = 1;
    let mut index = start;
    while index < source.len() {
        state.step()?;
        match source[index] {
            '\\' => {
                index += 2;
                continue;
            }
            '\'' => {
                index = find_char(source, index + 1, '\'').ok_or(Unparsed)? + 1;
                continue;
            }
            '"' => {
                index = skip_double_quoted(state, source, index + 1)?;
                continue;
            }
            '`' => {
                index = skip_backticks(state, source, index + 1)?;
                continue;
            }
            '$' if source.get(index + 1) == Some(&'(') => {
                index = match_paren(state, source, index + 2)?;
                continue;
            }
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Ok(index + 1);
                }
            }
            _ => {}
        }
        index += 1;
    }
    Err(Unparsed)
}

/// Whether the `((` whose first `(` is at `open` is arithmetic: its inner `(` must close right
/// before the outer one. Otherwise bash reads it as a substitution or subshell starting with a subshell.
fn is_arithmetic(state: &mut State, source: &[char], open: usize) -> Lex<bool> {
    let outer_end = match_paren(state, source, open + 1)?;
    let inner_end = match_paren(state, source, open + 2)?;
    Ok(inner_end + 1 == outer_end)
}

/// The unescaped body of a backtick substitution (bash removes `\` before `$`, `` ` `` and `\`).
fn unescape_backticks(body: &[char]) -> Vec<char> {
    let mut out = Vec::with_capacity(body.len());
    let mut index = 0;
    while index < body.len() {
        if body[index] == '\\' && matches!(body.get(index + 1), Some('$' | '`' | '\\')) {
            out.push(body[index + 1]);
            index += 2;
        } else {
            out.push(body[index]);
            index += 1;
        }
    }
    out
}

struct PendingHeredoc {
    delimiter: String,
    expands: bool,
    strip_tabs: bool,
}

#[derive(Clone, Copy, Default)]
struct Flags {
    dynamic: bool,
    quoted: bool,
}
const PLAIN: Flags = Flags {
    dynamic: false,
    quoted: false,
};
const DYNAMIC: Flags = Flags {
    dynamic: true,
    quoted: false,
};
const QUOTED: Flags = Flags {
    dynamic: false,
    quoted: true,
};

const RESERVED_LEADERS: &[&str] = &[
    "!", "{", "}", "if", "then", "else", "elif", "fi", "do", "done", "while", "until",
];
const UNSUPPORTED_LEADERS: &[&str] = &["case", "esac", "function", "coproc"];
const LOOP_HEADERS: &[&str] = &["for", "select"];
const SHELLS: &[&str] = &[
    "bash", "sh", "zsh", "dash", "ksh", "mksh", "ash", "yash", "posh", "fish", "csh", "tcsh",
];

/// What the next word is the target of: a plain redirection, or a here-document (with `<<-`).
#[derive(Clone, Copy)]
enum RedirectNext {
    Plain,
    Heredoc { strip_tabs: bool },
}

/// The lexer for one command list. `segments` collects every simple command found, in source
/// order, including the ones found inside substitutions.
struct Lexer<'a> {
    source: Vec<char>,
    index: usize,
    word: Option<Word>,
    words: Vec<Word>,
    redirect_next: Option<RedirectNext>,
    pending_heredocs: Vec<PendingHeredoc>,
    in_double_bracket: bool,
    depth: usize,
    text_depth: usize,
    segments: &'a mut Vec<Vec<Word>>,
    state: &'a mut State,
}

impl<'a> Lexer<'a> {
    fn new(
        source: Vec<char>,
        depth: usize,
        segments: &'a mut Vec<Vec<Word>>,
        state: &'a mut State,
    ) -> Lex<Self> {
        if depth > MAX_DEPTH {
            return Err(Unparsed);
        }
        Ok(Self {
            source,
            index: 0,
            word: None,
            words: Vec::new(),
            redirect_next: None,
            pending_heredocs: Vec::new(),
            in_double_bracket: false,
            depth,
            text_depth: 0,
            segments,
            state,
        })
    }

    fn at(&self, index: usize) -> Option<char> {
        self.source.get(index).copied()
    }

    fn run(&mut self) -> Lex<()> {
        while self.index < self.source.len() {
            self.state.step()?;
            let ch = self.source[self.index];
            let next = self.at(self.index + 1);
            if self.word.is_none() && ch == '#' {
                while self.index < self.source.len() && self.source[self.index] != '\n' {
                    self.index += 1;
                }
                continue;
            }
            match ch {
                ' ' | '\t' => {
                    self.end_word()?;
                    self.index += 1;
                    continue;
                }
                '\n' => {
                    self.end_segment()?;
                    self.index += 1;
                    self.read_heredoc_bodies()?;
                    continue;
                }
                '\\' => {
                    if next == Some('\n') {
                        self.index += 2;
                        continue;
                    }
                    let Some(next) = next else {
                        return Err(Unparsed);
                    };
                    self.append(&next.to_string(), QUOTED);
                    self.index += 2;
                    continue;
                }
                '\'' => {
                    let end = find_char(&self.source, self.index + 1, '\'').ok_or(Unparsed)?;
                    let text: String = self.source[self.index + 1..end].iter().collect();
                    self.append(&text, QUOTED);
                    self.index = end + 1;
                    continue;
                }
                '"' => {
                    self.read_double_quoted()?;
                    continue;
                }
                '`' => {
                    let end = skip_backticks(self.state, &self.source, self.index + 1)?;
                    let body = unescape_backticks(&self.source[self.index + 1..end - 1]);
                    self.nested(body)?;
                    self.append("", DYNAMIC);
                    self.index = end;
                    continue;
                }
                '$' => {
                    self.read_dollar()?;
                    continue;
                }
                _ => {}
            }
            if self.in_double_bracket
                && !self
                    .word
                    .as_ref()
                    .is_some_and(|word| !word.quoted && word.text == "]]")
            {
                self.append(&ch.to_string(), PLAIN);
                self.index += 1;
                continue;
            }
            if (ch == '<' || ch == '>') && next == Some('(') && self.word.is_none() {
                let end = match_paren(self.state, &self.source, self.index + 2)?;
                let body = self.source[self.index + 2..end - 1].to_vec();
                self.nested(body)?;
                self.append("", DYNAMIC);
                self.index = end;
                continue;
            }
            match ch {
                '<' | '>' => {
                    self.read_redirection()?;
                    continue;
                }
                '&' => {
                    if next == Some('>') {
                        self.end_word()?;
                        self.index += if self.at(self.index + 2) == Some('>') {
                            3
                        } else {
                            2
                        };
                        self.redirect_next = Some(RedirectNext::Plain);
                        continue;
                    }
                    self.end_segment()?;
                    self.index += if next == Some('&') { 2 } else { 1 };
                    continue;
                }
                '|' => {
                    self.end_segment()?;
                    self.index += if matches!(next, Some('|' | '&')) {
                        2
                    } else {
                        1
                    };
                    continue;
                }
                ';' => {
                    if matches!(next, Some(';' | '&')) {
                        return Err(Unparsed);
                    }
                    self.end_segment()?;
                    self.index += 1;
                    continue;
                }
                '(' => {
                    self.read_open_paren()?;
                    continue;
                }
                ')' => return Err(Unparsed),
                _ => {}
            }
            self.append(&ch.to_string(), PLAIN);
            if let Some(word) = self.word.as_mut() {
                if ch == '{' {
                    word.brace = true;
                }
                if matches!(ch, '*' | '?' | '[') {
                    word.glob = true;
                }
            }
            self.index += 1;
        }
        self.end_segment()?;
        // A here-document with no line after it has an empty body; there is nothing left to run.
        self.pending_heredocs.clear();
        Ok(())
    }

    fn append(&mut self, text: &str, flags: Flags) {
        let word = self.word.get_or_insert_with(Word::default);
        word.text.push_str(text);
        if flags.dynamic {
            word.dynamic = true;
        }
        if flags.quoted {
            word.quoted = true;
        }
        if flags.dynamic || flags.quoted {
            word.plain_ended = true;
        }
        if !word.plain_ended {
            word.plain_prefix = word.text.chars().count();
        }
    }

    fn nested(&mut self, source: Vec<char>) -> Lex<()> {
        let depth = self.depth + self.text_depth + 1;
        Lexer::new(source, depth, self.segments, self.state)?.run()
    }

    fn end_word(&mut self) -> Lex<()> {
        let Some(mut word) = self.word.take() else {
            return Ok(());
        };
        // Only `{a,b}` and `{1..3}` expand; `{}` (find's placeholder) and `{x}` stay literal.
        if word.brace && has_brace_expansion(&word.text) {
            word.dynamic = true;
        }
        // `[` and `[[` are the test command and keyword, not a pattern.
        if word.glob && word.text != "[" && word.text != "[[" {
            word.dynamic = true;
        }
        if let Some(redirect) = self.redirect_next.take() {
            if let RedirectNext::Heredoc { strip_tabs } = redirect {
                if word.dynamic {
                    return Err(Unparsed);
                }
                self.pending_heredocs.push(PendingHeredoc {
                    delimiter: word.text,
                    expands: !word.quoted,
                    strip_tabs,
                });
            }
            return Ok(());
        }
        if !word.quoted
            && word.text == "[["
            && self
                .words
                .iter()
                .all(|leader| !leader.quoted && RESERVED_LEADERS.contains(&leader.text.as_str()))
        {
            self.in_double_bracket = true;
        }
        if !word.quoted && word.text == "]]" && self.in_double_bracket {
            self.in_double_bracket = false;
        }
        self.words.push(word);
        Ok(())
    }

    fn end_segment(&mut self) -> Lex<()> {
        self.end_word()?;
        // A redirection operator with no target is a syntax error; bash runs nothing.
        if self.redirect_next.is_some() || self.in_double_bracket {
            return Err(Unparsed);
        }
        if !self.words.is_empty() {
            self.segments.push(std::mem::take(&mut self.words));
        }
        Ok(())
    }

    fn read_double_quoted(&mut self) -> Lex<()> {
        self.append("", QUOTED);
        self.index += 1;
        while self.index < self.source.len() {
            let ch = self.source[self.index];
            match ch {
                '"' => {
                    self.index += 1;
                    return Ok(());
                }
                '\\' => {
                    let Some(next) = self.at(self.index + 1) else {
                        return Err(Unparsed);
                    };
                    if next == '\n' {
                        self.index += 2;
                        continue;
                    }
                    let text = if "$`\"\\".contains(next) {
                        next.to_string()
                    } else {
                        format!("\\{next}")
                    };
                    self.append(&text, PLAIN);
                    self.index += 2;
                }
                '`' => {
                    let end = skip_backticks(self.state, &self.source, self.index + 1)?;
                    let body = unescape_backticks(&self.source[self.index + 1..end - 1]);
                    self.nested(body)?;
                    self.append("", DYNAMIC);
                    self.index = end;
                }
                '$' => self.read_dollar()?,
                _ => {
                    self.append(&ch.to_string(), PLAIN);
                    self.index += 1;
                }
            }
        }
        Err(Unparsed)
    }

    /// `$…` in or out of double quotes: substitutions are searched, every expansion is dynamic.
    fn read_dollar(&mut self) -> Lex<()> {
        let next = self.at(self.index + 1);
        if next == Some('(')
            && self.at(self.index + 2) == Some('(')
            && is_arithmetic(self.state, &self.source, self.index + 1)?
        {
            let end = match_paren(self.state, &self.source, self.index + 2)?;
            let body = self.source[self.index + 3..end - 2].to_vec();
            self.nested_text(&body)?;
            self.append("", DYNAMIC);
            self.index = end;
            return Ok(());
        }
        if next == Some('(') {
            let end = match_paren(self.state, &self.source, self.index + 2)?;
            let body = self.source[self.index + 2..end - 1].to_vec();
            self.nested(body)?;
            self.append("", DYNAMIC);
            self.index = end;
            return Ok(());
        }
        if next == Some('{') {
            let end = match_brace(self.state, &self.source, self.index + 2)?;
            let body = self.source[self.index + 2..end - 1].to_vec();
            self.nested_text(&body)?;
            self.append("", DYNAMIC);
            self.index = end;
            return Ok(());
        }
        if next == Some('\'') {
            // ANSI-C quoting decodes escapes, so its value is unknown to this lexer.
            let mut index = self.index + 2;
            while index < self.source.len() && self.source[index] != '\'' {
                index += if self.source[index] == '\\' { 2 } else { 1 };
            }
            if index >= self.source.len() {
                return Err(Unparsed);
            }
            self.append(
                "",
                Flags {
                    dynamic: true,
                    quoted: true,
                },
            );
            self.index = index + 1;
            return Ok(());
        }
        if let Some(next) = next.filter(|next| is_name_char(*next) || "@*#?$!-".contains(*next)) {
            let mut index = self.index + 1;
            if is_name_char(next) && !next.is_ascii_digit() {
                while index < self.source.len() && is_name_char(self.source[index]) {
                    index += 1;
                }
            } else {
                index += 1;
            }
            self.append("", DYNAMIC);
            self.index = index;
            return Ok(());
        }
        self.append("$", PLAIN);
        self.index += 1;
        Ok(())
    }

    /// Searches text that is not a command list (a `${…}` body, a here-document) for substitutions.
    fn nested_text(&mut self, text: &[char]) -> Lex<()> {
        self.text_depth += 1;
        let result = if self.depth + self.text_depth > MAX_DEPTH {
            Err(Unparsed)
        } else {
            self.scan_text(text)
        };
        self.text_depth -= 1;
        result
    }

    fn scan_text(&mut self, text: &[char]) -> Lex<()> {
        let mut index = 0;
        while index < text.len() {
            let ch = text[index];
            let next = text.get(index + 1).copied();
            if ch == '\\' {
                index += 2;
                continue;
            }
            if ch == '`' {
                let end = skip_backticks(self.state, text, index + 1)?;
                self.nested(unescape_backticks(&text[index + 1..end - 1]))?;
                index = end;
                continue;
            }
            if ch == '$' && next == Some('(') {
                let end = match_paren(self.state, text, index + 2)?;
                if text.get(index + 2) == Some(&'(') && is_arithmetic(self.state, text, index + 1)?
                {
                    self.nested_text(&text[index + 3..end - 2])?;
                } else {
                    self.nested(text[index + 2..end - 1].to_vec())?;
                }
                index = end;
                continue;
            }
            if ch == '$' && next == Some('{') {
                let end = match_brace(self.state, text, index + 2)?;
                self.nested_text(&text[index + 2..end - 1])?;
                index = end;
                continue;
            }
            index += 1;
        }
        Ok(())
    }

    fn read_redirection(&mut self) -> Lex<()> {
        // A word made only of digits right before the operator is a file descriptor, not an argument.
        if self
            .word
            .as_ref()
            .is_some_and(|word| !word.quoted && !word.dynamic && is_digits(&word.text))
        {
            self.word = None;
        } else {
            self.end_word()?;
        }
        let ch = self.source[self.index];
        let next = self.at(self.index + 1);
        if ch == '<' && next == Some('<') && self.at(self.index + 2) == Some('<') {
            self.index += 3;
            self.redirect_next = Some(RedirectNext::Plain);
            return Ok(());
        }
        if ch == '<' && next == Some('<') {
            let strip_tabs = self.at(self.index + 2) == Some('-');
            self.index += if strip_tabs { 3 } else { 2 };
            self.redirect_next = Some(RedirectNext::Heredoc { strip_tabs });
            return Ok(());
        }
        self.index += 1;
        if (ch == '>' && matches!(next, Some('>' | '|' | '&')))
            || (ch == '<' && matches!(next, Some('>' | '&')))
        {
            self.index += 1;
        }
        self.redirect_next = Some(RedirectNext::Plain);
        Ok(())
    }

    fn read_open_paren(&mut self) -> Lex<()> {
        if let Some(word) = &self.word {
            // `name=( … )` is an array assignment; any other `word(` is a function definition or a syntax error.
            if word.quoted || !is_array_assignment_head(&word.text) {
                return Err(Unparsed);
            }
            let end = match_paren(self.state, &self.source, self.index + 1)?;
            let body = self.source[self.index + 1..end - 1].to_vec();
            self.nested_text(&body)?;
            self.append("", DYNAMIC);
            self.index = end;
            return Ok(());
        }
        if self.at(self.index + 1) == Some('(')
            && is_arithmetic(self.state, &self.source, self.index)?
        {
            let end = match_paren(self.state, &self.source, self.index + 1)?;
            let body = self.source[self.index + 2..end - 2].to_vec();
            self.nested_text(&body)?;
            self.index = end;
            return Ok(());
        }
        let end = match_paren(self.state, &self.source, self.index + 1)?;
        self.end_segment()?;
        let body = self.source[self.index + 1..end - 1].to_vec();
        self.nested(body)?;
        self.index = end;
        Ok(())
    }

    fn read_heredoc_bodies(&mut self) -> Lex<()> {
        for heredoc in std::mem::take(&mut self.pending_heredocs) {
            let mut body: Vec<String> = Vec::new();
            while self.index < self.source.len() {
                let line_end = find_char(&self.source, self.index, '\n');
                let end = line_end.unwrap_or(self.source.len());
                let raw: String = self.source[self.index..end].iter().collect();
                self.index = line_end.map_or(self.source.len(), |line_end| line_end + 1);
                let line = if heredoc.strip_tabs {
                    raw.trim_start_matches('\t').to_owned()
                } else {
                    raw
                };
                if line == heredoc.delimiter {
                    break;
                }
                body.push(line);
            }
            if heredoc.expands {
                let text: Vec<char> = body.join("\n").chars().collect();
                self.nested_text(&text)?;
            }
        }
        Ok(())
    }
}

/// `/\{[^}]*(,|\.\.)[^}]*\}/`: a brace pair holding `,` or `..`.
fn has_brace_expansion(text: &str) -> bool {
    let mut rest = text;
    while let Some(open) = rest.find('{') {
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            return false;
        };
        let inner = &after[..close];
        if inner.contains(',') || inner.contains("..") {
            return true;
        }
        rest = after;
    }
    false
}

/// `/^[A-Za-z_][A-Za-z0-9_]*\+?=$/`.
fn is_array_assignment_head(text: &str) -> bool {
    let body = text.strip_suffix('=').unwrap_or_default();
    let body = body.strip_suffix('+').unwrap_or(body);
    let mut chars = body.chars();
    text.ends_with('=')
        && chars
            .next()
            .is_some_and(|ch| ch.is_ascii_alphabetic() || ch == '_')
        && chars.all(is_name_char)
}

/// Options a wrapper takes: `Flag` = a flag alone, `Value` = a flag followed by one value.
#[derive(Clone, Copy, PartialEq, Eq)]
enum OptionKind {
    Flag,
    Value,
}
struct OptionTable {
    flags: &'static str,
    values: &'static str,
}
impl OptionTable {
    fn get(&self, name: &str) -> Option<OptionKind> {
        if self
            .values
            .split(' ')
            .any(|value| !value.is_empty() && value == name)
        {
            return Some(OptionKind::Value);
        }
        self.flags
            .split(' ')
            .any(|flag| !flag.is_empty() && flag == name)
            .then_some(OptionKind::Flag)
    }
}
const fn opts(flags: &'static str, values: &'static str) -> OptionTable {
    OptionTable { flags, values }
}

/// Skips a wrapper's options and returns the index of the wrapped command's first word, or
/// `words.len()` when there is none. An option the table does not list makes the command
/// unparsed: guessing whether it takes a value could hide the wrapped command.
fn skip_options(words: &[Word], start: usize, table: &OptionTable) -> Lex<usize> {
    let mut index = start;
    while index < words.len() {
        let word = &words[index];
        if word.dynamic {
            return Err(Unparsed);
        }
        let text = word.text.as_str();
        if text == "--" {
            return Ok(index + 1);
        }
        if !text.starts_with('-') || text == "-" {
            return Ok(index);
        }
        if text.starts_with("--") {
            let (name, has_value) = text
                .split_once('=')
                .map_or((text, false), |(name, _)| (name, true));
            let kind = table.get(name).ok_or(Unparsed)?;
            index += if kind == OptionKind::Value && !has_value {
                2
            } else {
                1
            };
            continue;
        }
        match table.get(text) {
            Some(OptionKind::Value) => {
                index += 2;
                continue;
            }
            Some(OptionKind::Flag) => {
                index += 1;
                continue;
            }
            None => {}
        }
        // A cluster of single-letter flags (`-lc`), where only the last may take a value (`-un`).
        let letters: Vec<char> = text[1..].chars().collect();
        let mut consumed = 1;
        for (position, letter) in letters.iter().enumerate() {
            match table.get(&format!("-{letter}")) {
                None => {
                    // Numeric options such as `nice -5`.
                    if is_digits(&text[1..]) && table.get("-N") == Some(OptionKind::Flag) {
                        break;
                    }
                    return Err(Unparsed);
                }
                Some(OptionKind::Value) => {
                    if position == letters.len() - 1 {
                        consumed = 2;
                    }
                    break;
                }
                Some(OptionKind::Flag) => {}
            }
        }
        index += consumed;
    }
    Ok(index)
}

fn wrapper_options(name: &str) -> Option<OptionTable> {
    Some(match name {
        "command" => opts("-p -v -V", ""),
        "builtin" => opts("", ""),
        "exec" => opts("-c -l", "-a"),
        "nohup" => opts("", ""),
        "time" => opts(
            "-p -l -v -a --portability --verbose --append",
            "-o -f --output --format",
        ),
        "nice" => opts("-N", "-n --adjustment"),
        "stdbuf" => opts("", "-i -o -e --input --output --error"),
        "caffeinate" => opts("-d -i -m -s -u", "-t -w"),
        "sudo" => opts(
            "-A -b -E -H -K -k -n -P -S -V -v --askpass --background --preserve-env --set-home --non-interactive --stdin",
            "-u -g -h -p -C -D -r -t -U -T --user --group --host --prompt --close-from --chdir --role --type --other-user --command-timeout",
        ),
        "doas" => opts("-n -s", "-u -C"),
        "setsid" => opts("-c -f -w --ctty --fork --wait", ""),
        "ionice" => opts("-t --ignore", "-c -n --class --classdata"),
        _ => return None,
    })
}

const ENV_OPTIONS: OptionTable = opts(
    "-i -0 -v --ignore-environment --null --debug",
    "-u -C --unset --chdir",
);
const TIMEOUT_OPTIONS: OptionTable = opts(
    "-v --preserve-status --foreground --verbose",
    "-s -k --signal --kill-after",
);
const XARGS_OPTIONS: OptionTable = opts(
    "-0 -r -t -p -x -o --null --no-run-if-empty --verbose --interactive --exit --open-tty",
    "-I -L -n -P -s -d -E -a --max-lines --max-args --max-procs --max-chars --delimiter --eof --arg-file --replace --process-slot-var",
);
const SHELL_OPTIONS: OptionTable = opts(
    "-e -u -x -v -f -n -l -i -s -p -a -b -h -k -m -B -C -E -H -P -T --login --norc --noprofile --posix --restricted --noediting -c",
    "-o -O --rcfile --init-file",
);

fn path_basename(text: &str) -> &str {
    text.rsplit('/').next().unwrap_or(text)
}

/// Turns one simple command's words into the command lines it runs: the wrapper's own name first,
/// then the wrapped command, recursively. Every name is reduced to its basename.
fn expand_simple_command(state: &mut State, input: &[Word], out: &mut Vec<Vec<String>>) -> Lex<()> {
    if state.wrapper_depth >= MAX_WRAPPER_DEPTH {
        return Err(Unparsed);
    }
    state.wrapper_depth += 1;
    let result = expand_one_command(state, input, out);
    state.wrapper_depth -= 1;
    result
}

fn expand_one_command(state: &mut State, input: &[Word], out: &mut Vec<Vec<String>>) -> Lex<()> {
    // Leading assignments and reserved words do not name the command.
    let mut start = 0;
    while let Some(word) = input.get(start) {
        if !word.quoted && RESERVED_LEADERS.contains(&word.text.as_str()) {
            start += 1;
            continue;
        }
        if !word.quoted && UNSUPPORTED_LEADERS.contains(&word.text.as_str()) {
            return Err(Unparsed);
        }
        if is_assignment(word) {
            start += 1;
            continue;
        }
        break;
    }
    let words = &input[start..];
    let Some(first) = words.first() else {
        return Ok(());
    };
    if !first.quoted && LOOP_HEADERS.contains(&first.text.as_str()) {
        // `for NAME [in WORDS]` and `for (( … ))` run nothing themselves; substitutions in them were already found.
        if words.len() <= 2 || words[2].text == "in" {
            return Ok(());
        }
        return Err(Unparsed);
    }
    if first.dynamic || first.text.is_empty() {
        return Err(Unparsed);
    }
    let name = path_basename(&first.text).to_owned();
    if name.is_empty() {
        return Err(Unparsed);
    }
    let rest = &words[1..];
    let line: Vec<String> = std::iter::once(name.clone())
        .chain(rest.iter().map(|word| word.text.clone()))
        .collect();

    if SHELLS.contains(&name.as_str()) {
        out.push(vec![name]);
        let Some(command_index) = shell_command_index(rest)? else {
            out.push(line);
            return Ok(());
        };
        let script = rest
            .get(command_index)
            .filter(|script| !script.dynamic)
            .ok_or(Unparsed)?;
        return expand_script(state, &script.text, out);
    }
    let kind = name.clone();
    match kind.as_str() {
        "eval" => {
            out.push(vec![name]);
            if rest.iter().any(|word| word.dynamic) {
                return Err(Unparsed);
            }
            let args = if rest.first().is_some_and(|word| word.text == "--") {
                &rest[1..]
            } else {
                rest
            };
            let script = args
                .iter()
                .map(|word| word.text.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            return expand_script(state, &script, out);
        }
        "su" => {
            out.push(vec![name]);
            return match su_command(rest)? {
                Some(script) => expand_script(state, &script, out),
                None => Ok(()),
            };
        }
        "trap" => {
            out.push(vec![name]);
            let Some(script) = rest.iter().find(|word| !word.text.starts_with('-')) else {
                return Ok(());
            };
            if script.dynamic {
                return Err(Unparsed);
            }
            return expand_script(state, &script.text, out);
        }
        "env" => {
            out.push(vec![name]);
            if rest.iter().any(|word| {
                !word.dynamic && (word.text == "-S" || word.text.starts_with("--split-string"))
            }) {
                return Err(Unparsed);
            }
            let mut index = skip_options(rest, 0, &ENV_OPTIONS)?;
            // env itself, not the shell, reads these: any NAME=VALUE word, quoted or not, is an assignment.
            while index < rest.len()
                && !rest[index].dynamic
                && rest[index].text.find('=').is_some_and(|at| at > 0)
            {
                index += 1;
            }
            return expand_simple_command(state, &rest[index..], out);
        }
        "timeout" => {
            out.push(vec![name]);
            let index = skip_options(rest, 0, &TIMEOUT_OPTIONS)?;
            // The first operand is the duration.
            return expand_simple_command(state, rest.get(index + 1..).unwrap_or_default(), out);
        }
        "xargs" => {
            out.push(vec![name]);
            let index = skip_options(rest, 0, &XARGS_OPTIONS)?;
            return expand_simple_command(state, &rest[index..], out);
        }
        "find" => {
            out.push(line);
            // find reads its words after quote removal, so a quoted action is still an action.
            if rest.iter().any(|word| word.dynamic) {
                return Err(Unparsed);
            }
            let mut index = 0;
            while index < rest.len() {
                if ["-exec", "-execdir", "-ok", "-okdir"].contains(&rest[index].text.as_str()) {
                    let mut end = index + 1;
                    while end < rest.len() && rest[end].text != ";" && rest[end].text != "+" {
                        end += 1;
                    }
                    if end >= rest.len() {
                        return Err(Unparsed);
                    }
                    expand_simple_command(state, &rest[index + 1..end], out)?;
                    index = end;
                }
                index += 1;
            }
            return Ok(());
        }
        _ => {}
    }
    if let Some(table) = wrapper_options(&name) {
        out.push(vec![name]);
        let index = skip_options(rest, 0, &table)?;
        return expand_simple_command(state, &rest[index..], out);
    }
    out.push(line);
    Ok(())
}

fn expand_script(state: &mut State, script: &str, out: &mut Vec<Vec<String>>) -> Lex<()> {
    for segment in lex_segments(state, script)? {
        expand_simple_command(state, &segment, out)?;
    }
    Ok(())
}

/// The command string `su` runs (`-c`, `--command`, `--session-command`, or `c` in a flag cluster), if any.
fn su_command(rest: &[Word]) -> Lex<Option<String>> {
    for (index, word) in rest.iter().enumerate() {
        let text = word.text.as_str();
        let long = ["--session-command", "--command"]
            .iter()
            .find_map(|prefix| {
                let tail = text.strip_prefix(prefix)?;
                if tail.is_empty() {
                    Some(None)
                } else {
                    tail.strip_prefix('=').map(Some)
                }
            });
        if let Some(attached) = long {
            let (value, source) = match attached {
                Some(value) => (Some(value.to_owned()), Some(word)),
                None => (
                    rest.get(index + 1).map(|next| next.text.clone()),
                    rest.get(index + 1),
                ),
            };
            return match (value, source) {
                (Some(value), Some(source)) if !source.dynamic => Ok(Some(value)),
                _ => Err(Unparsed),
            };
        }
        // `/^-[A-Za-z]*c/` outside long options.
        if !text.starts_with("--")
            && let Some(cluster) = text.strip_prefix('-')
            && let Some(at) = cluster.find(|ch: char| !ch.is_ascii_alphabetic() || ch == 'c')
            && cluster.as_bytes()[at] == b'c'
        {
            let attached = &text[text.find('c').map_or(text.len(), |at| at + 1)..];
            let source = if attached.is_empty() {
                rest.get(index + 1)
            } else {
                Some(word)
            };
            let source = source.filter(|source| !source.dynamic).ok_or(Unparsed)?;
            return Ok(Some(if attached.is_empty() {
                source.text.clone()
            } else {
                attached.to_owned()
            }));
        }
    }
    Ok(None)
}

/// The index (in `rest`) of a shell's `-c` command string, or `None` when it reads a file or stdin.
fn shell_command_index(rest: &[Word]) -> Lex<Option<usize>> {
    let mut index = 0;
    let mut has_command_flag = false;
    while let Some(word) = rest.get(index) {
        if word.dynamic {
            return Err(Unparsed);
        }
        let text = word.text.as_str();
        if text == "--" || text == "-" {
            index += 1;
            break;
        }
        if !text.starts_with('-') && !text.starts_with('+') {
            break;
        }
        if text.starts_with("--") {
            let kind = SHELL_OPTIONS
                .get(text.split('=').next().unwrap_or(text))
                .ok_or(Unparsed)?;
            index += if kind == OptionKind::Value && !text.contains('=') {
                2
            } else {
                1
            };
            continue;
        }
        let letters = &text[1..];
        if letters.contains('c') {
            has_command_flag = true;
        }
        if letters
            .chars()
            .any(|letter| SHELL_OPTIONS.get(&format!("-{letter}")).is_none())
        {
            return Err(Unparsed);
        }
        index += if letters.contains('o') || letters.contains('O') {
            2
        } else {
            1
        };
    }
    Ok(has_command_flag.then_some(index))
}

fn lex_segments(state: &mut State, command: &str) -> Lex<Vec<Vec<Word>>> {
    let mut segments = Vec::new();
    Lexer::new(command.chars().collect(), 0, &mut segments, state)?.run()?;
    Ok(segments)
}

/// Every simple command `command` would run, each as its normalized words (command name first),
/// in source order; or `Unparsed` when any part of the command is outside what this lexer understands.
pub fn analyze_bash_command(command: &str) -> BashCommandAnalysis {
    let mut state = State {
        steps: 0,
        wrapper_depth: 0,
    };
    // `command.length` counts UTF-16 units upstream; the char count is never larger.
    let result = if command.encode_utf16().count() > MAX_COMMAND_LENGTH {
        Err(Unparsed)
    } else {
        (|| -> Lex<Vec<Vec<String>>> {
            let mut out = Vec::new();
            for segment in lex_segments(&mut state, command)? {
                expand_simple_command(&mut state, &segment, &mut out)?;
            }
            Ok(out)
        })()
    };
    LAST_STEPS.with(|steps| steps.set(state.steps));
    // Fail closed: a parse failure or the depth or step budget all mean `Unparsed`, which no
    // command-specific rule can allow.
    match result {
        Ok(segments) => BashCommandAnalysis::Parsed { segments },
        Err(Unparsed) => BashCommandAnalysis::Unparsed,
    }
}

/// Lexer steps the last `analyze_bash_command` call on this thread used (for scaling tests).
pub fn last_analysis_steps() -> usize {
    LAST_STEPS.with(Cell::get)
}

/// Candidate command names for a command the lexer could not parse, used only so a deny rule
/// still matches: every plain word, at every position, read as the start of a command.
pub fn best_effort_command_lines(command: &str) -> Vec<Vec<String>> {
    let words: Vec<String> = command
        .split(|ch: char| ch.is_whitespace() || ";&|()<>`$'\"{}\\".contains(ch))
        .filter(|word| {
            !word.is_empty()
                && word
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || "._+/-".contains(ch))
        })
        .map(|word| path_basename(word).to_owned())
        .filter(|word| !word.is_empty())
        .collect();
    (0..words.len())
        .map(|index| words[index..(index + CANDIDATE_WORDS).min(words.len())].to_vec())
        .collect()
}
