//! Port of `permission-system/auto-shell-segments.ts` (senpi 903b8336).
//!
//! Splits a shell command into the simple commands the `auto` preset can judge one by one. Only
//! plain words joined by `;` or `&&` are understood. Quotes, escapes, expansions (`$`, `~`, globs),
//! redirects, pipes, background jobs, subshells, comments and line breaks return `None`, and the
//! caller asks the user.

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShellWord {
    pub text: String,
}

pub type ShellSegment = Vec<ShellWord>;

/// `/^[A-Za-z0-9_\-.,/=+ \t;&]*$/`
fn is_plain_command(command: &str) -> bool {
    command
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || "_-.,/=+ \t;&".contains(ch))
}

fn words(part: &str) -> Vec<ShellWord> {
    part.split([' ', '\t'])
        .filter(|text| !text.is_empty())
        .map(|text| ShellWord {
            text: text.to_owned(),
        })
        .collect()
}

pub fn split_shell_segments(command: &str) -> Option<Vec<ShellSegment>> {
    if !is_plain_command(command) {
        return None;
    }
    let chains: Vec<&str> = command.split("&&").collect();
    if chains.iter().any(|chain| chain.contains('&')) {
        return None;
    }
    let mut segments = Vec::new();
    for (index, chain) in chains.iter().enumerate() {
        let parts: Vec<Vec<ShellWord>> = chain.split(';').map(words).collect();
        if index > 0 && parts.first().is_some_and(Vec::is_empty) {
            return None;
        }
        if index < chains.len() - 1 && parts.last().is_some_and(Vec::is_empty) {
            return None;
        }
        segments.extend(parts.into_iter().filter(|part| !part.is_empty()));
    }
    Some(segments)
}
