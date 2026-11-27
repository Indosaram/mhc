//! Port of `packages/coding-agent/src/cli/kill-guard-command.ts` (senpi#5349).
//!
//! The hidden `__kill-guard` subcommand the kill guard's runtime shims call. Every CLI entry
//! answers it first, before agent-dir initialization, migrations, provider registration or hosts.

use maho_core::kill_guard::check_command::{KILL_GUARD_COMMAND, run_kill_guard_check_command};

/// Runs the hidden `__kill-guard` subcommand when `args[0] == "__kill-guard"`, returning its
/// exit code; returns `None` for any other command line.
pub fn dispatch_kill_guard_command(args: &[String]) -> Option<i32> {
    if args.first().map(String::as_str) != Some(KILL_GUARD_COMMAND) {
        return None;
    }
    Some(run_kill_guard_check_command())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ignores_other_commands() {
        assert_eq!(dispatch_kill_guard_command(&[]), None);
        assert_eq!(dispatch_kill_guard_command(&["--help".into()]), None);
        assert_eq!(dispatch_kill_guard_command(&["doctor".into()]), None);
    }
}
