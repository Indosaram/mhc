use std::sync::LazyLock;

use regex::Regex;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandMatch {
    pub full_match: String,
    pub command: String,
    pub start: usize,
    pub end: usize,
}

static COMMAND_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new("!\u{60}([^\u{60}]+)\u{60}").unwrap_or_else(|error| panic!("{error}"))
});

pub fn find_embedded_commands(text: &str) -> Vec<CommandMatch> {
    COMMAND_PATTERN
        .captures_iter(text)
        .filter_map(|captures| {
            let whole = captures.get(0)?;
            Some(CommandMatch {
                full_match: whole.as_str().to_string(),
                command: captures.get(1)?.as_str().to_string(),
                start: whole.start(),
                end: whole.end(),
            })
        })
        .collect()
}
