//! Port of senpi `cli/piped-stdin-options.ts`.
use super::args::Args;

pub const PIPED_STDIN_SKIP_HINT: &str = "stdin ignored because a prompt was given and no input arrived within 2000 ms; pass - to read stdin";

/// Whether a prompt argument (message or `@file`) was given, and whether `-` forces the read.
pub struct PipedStdinArgs {
    pub has_prompt_argument: bool,
    pub force_read: bool,
}

pub fn piped_stdin_options(parsed: &Args) -> PipedStdinArgs {
    PipedStdinArgs {
        has_prompt_argument: !parsed.messages.is_empty() || !parsed.file_args.is_empty(),
        force_read: parsed.read_stdin,
    }
}

/// `onSkip`: tell the user on stderr why stdin was ignored.
pub fn write_skip_hint(stderr: &mut dyn std::io::Write) {
    let _ = writeln!(stderr, "{PIPED_STDIN_SKIP_HINT}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_and_force_flags_follow_the_parsed_args() {
        let none = piped_stdin_options(&Args::default());
        assert!(!none.has_prompt_argument && !none.force_read);
        let args = Args {
            file_args: vec!["a.md".into()],
            read_stdin: true,
            ..Args::default()
        };
        let some = piped_stdin_options(&args);
        assert!(some.has_prompt_argument && some.force_read);
    }

    #[test]
    fn skip_hint_goes_to_the_writer_with_a_newline() {
        let mut out = Vec::new();
        write_skip_hint(&mut out);
        assert_eq!(
            String::from_utf8(out).unwrap_or_default(),
            format!("{PIPED_STDIN_SKIP_HINT}\n")
        );
    }
}
