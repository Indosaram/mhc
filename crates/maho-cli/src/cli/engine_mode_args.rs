//! Port of `packages/coding-agent/src/cli/engine-mode-args.ts` (senpi#5840).

use super::args::{Args, Diagnostic, Mode};

/// `--hand` and `--headless` start the engine on its own endpoint, speaking its own protocol:
/// both run in RPC mode (nothing reaches stdout), `--headless` as a multi-session host.
/// What would ask the engine for a prompt, a print run or a text/json stream is refused.
pub fn apply_engine_mode_args(args: &mut Args) {
    if !args.hand && !args.headless {
        return;
    }
    if args.hand && args.headless {
        args.diagnostics.push(Diagnostic {
            error: true,
            message: "--hand and --headless cannot be combined".to_owned(),
        });
        return;
    }
    let flag = if args.hand { "--hand" } else { "--headless" };
    if args.print || args.read_stdin || !args.messages.is_empty() || !args.file_args.is_empty() {
        args.diagnostics.push(Diagnostic {
            error: true,
            message: format!(
                "{flag} serves its own endpoint; omit --print, stdin input, prompt arguments and @files"
            ),
        });
    }
    if let Some(mode) = &args.mode
        && *mode != Mode::Rpc
    {
        let mode_str = match mode {
            Mode::Text => "text",
            Mode::Json => "json",
            Mode::Rpc => "rpc",
        };
        args.diagnostics.push(Diagnostic {
            error: true,
            message: format!(
                "{flag} speaks its own protocol and cannot be combined with --mode {mode_str}"
            ),
        });
    }
    if args.hand && (args.listen.is_some() || args.multi_session) {
        args.diagnostics.push(Diagnostic {
            error: true,
            message: "--hand listens on its own endpoint; omit --listen and --multi-session"
                .to_owned(),
        });
    }
    args.mode = Some(Mode::Rpc);
    if args.headless {
        args.multi_session = true;
    }
}

#[cfg(test)]
mod tests {
    use super::super::args::{Mode, parse_args};

    fn parse(argv: &[&str]) -> super::Args {
        let owned: Vec<String> = argv.iter().map(|s| (*s).to_owned()).collect();
        parse_args(&owned, false).expect("parse args")
    }

    #[test]
    fn headless_sets_rpc_and_multi_session_and_rejects_conflicts() {
        let ok = parse(&["--headless"]);
        assert!(ok.diagnostics.is_empty());
        assert_eq!(ok.mode, Some(Mode::Rpc));
        assert!(ok.multi_session);

        let both = parse(&["--hand", "--headless"]);
        assert!(
            both.diagnostics
                .iter()
                .any(|d| d.message.contains("cannot be combined"))
        );

        let hand_listen = parse(&["--hand", "--listen", "stdio://"]);
        assert!(
            hand_listen
                .diagnostics
                .iter()
                .any(|d| d.message.contains("omit --listen and --multi-session"))
        );
    }
}
