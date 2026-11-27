//! Port of `packages/coding-agent/src/cli/attach-args.ts`.

use super::args::{Args, FlagValue, Mode};

/// Attachment flags are resolved before mode selection or any local session initialization.
pub fn apply_attach_args(args: &mut Args) {
    let Some(value) = args.unknown_flags.remove("attach") else {
        return;
    };
    let mut errors = Vec::new();
    let attach = match value {
        FlagValue::String(text) if !text.is_empty() => text,
        _ => {
            args.diagnostics.push(super::args::Diagnostic {
                error: true,
                message: "--attach requires a local RPC endpoint address".to_owned(),
            });
            return;
        }
    };
    args.attach = Some(attach);
    if args.mode.as_ref().is_some_and(|mode| *mode != Mode::Text) {
        errors.push("--attach is interactive and cannot be combined with --mode rpc or json");
    }
    if args.session.is_none() && !args.help && !args.version {
        errors.push("--attach requires --session <ref>");
    }
    if args.print || args.read_stdin || !args.messages.is_empty() || !args.file_args.is_empty() {
        errors.push(
            "--attach needs interactive input; omit --print, stdin input, prompt arguments and @files",
        );
    }
    if args.continue_session
        || args.resume
        || args.fork.is_some()
        || args.rebind.is_some()
        || args.session_id.is_some()
        || args.no_session
        || args.listen.is_some()
        || args.multi_session
    {
        errors.push(
            "--attach selects an existing session; omit session creation, resume and listener options",
        );
    }
    if args.provider.is_some()
        || args.model.is_some()
        || args.api_key.is_some()
        || args.system_prompt.is_some()
        || !args.append_system_prompt.is_empty()
        || args.export.is_some()
    {
        errors.push(
            "--attach uses the endpoint's session configuration; omit model, prompt configuration and export options",
        );
    }
    for message in errors {
        args.diagnostics.push(super::args::Diagnostic {
            error: true,
            message: message.to_owned(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::super::args::{Diagnostic, parse_args};

    fn diagnostics(argv: &[&str]) -> Vec<String> {
        let owned: Vec<String> = argv.iter().map(|s| (*s).to_owned()).collect();
        parse_args(&owned, false)
            .expect("parse args")
            .diagnostics
            .into_iter()
            .map(|Diagnostic { message, .. }| message)
            .collect()
    }

    #[test]
    fn accepts_valid_attach_args_and_rejects_conflicts() {
        assert!(diagnostics(&["--attach", "unix:///tmp/a.sock", "--session", "s1"]).is_empty());
        assert!(
            diagnostics(&["--attach", "unix:///tmp/a.sock"])
                .iter()
                .any(|m| m.contains("--attach requires --session <ref>"))
        );
        assert!(
            diagnostics(&[
                "--attach",
                "unix:///tmp/a.sock",
                "--session",
                "s1",
                "--mode",
                "rpc"
            ])
            .iter()
            .any(|m| m.contains("cannot be combined with --mode rpc or json"))
        );
    }
}
