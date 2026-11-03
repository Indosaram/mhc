//! Rust port of `packages/telemetry-core/src/terminal-program.ts`.

pub const KNOWN_TERMINAL_PROGRAMS: [&str; 16] = [
    "Apple_Terminal",
    "iTerm.app",
    "vscode",
    "cursor",
    "windsurf",
    "zed",
    "WezTerm",
    "ghostty",
    "tmux",
    "Hyper",
    "WarpTerminal",
    "kitty",
    "alacritty",
    "Tabby",
    "rio",
    "mintty",
];

/// A known `TERM_PROGRAM` name as is, any other non-empty value as `other`, unset or empty as `None`.
pub fn to_telemetry_terminal_program(term_program: Option<&str>) -> Option<&'static str> {
    let value = term_program.filter(|value| !value.is_empty())?;
    Some(
        KNOWN_TERMINAL_PROGRAMS
            .iter()
            .find(|known| **known == value)
            .copied()
            .unwrap_or("other"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_known_unknown_and_empty() {
        for known in KNOWN_TERMINAL_PROGRAMS {
            assert_eq!(to_telemetry_terminal_program(Some(known)), Some(known));
        }
        for unknown in [
            "/home/sentinel-user/bin/x",
            "my-private-terminal",
            "iterm.app",
            " vscode ",
        ] {
            assert_eq!(to_telemetry_terminal_program(Some(unknown)), Some("other"));
        }
        assert_eq!(to_telemetry_terminal_program(Some("")), None);
        assert_eq!(to_telemetry_terminal_program(None), None);
    }
}
