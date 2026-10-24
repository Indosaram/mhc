//! Internal process roles (senpi#3566, plan todo 4; senpi `internal-role.ts`). Called first in every entry point, so a
//! process the engine started as a tool child becomes one before version dispatch, argument parsing or the CLI module
//! graph. Without its channel environment a role process exits 70 at once instead of falling through to the ordinary CLI.
use maho_tool_child::channel::{TOOL_CHILD_ROLE_ENV, channel_env_present};
use maho_tool_child::launch_env_names::process_env;

/// Exit status of a role process that cannot do its role (`process.exit(70)`).
pub const INTERNAL_ROLE_EXIT: i32 = 70;

/// What the process environment asks of this process, decided from the role value and the channel variables alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InternalRoleDecision {
    /// No role: the ordinary CLI runs.
    None,
    /// Windows only: starts one detached command outside the child's Job Object.
    LaunchShim,
    /// The tool child, with its channel environment present.
    ToolChild,
    /// Exit [`INTERNAL_ROLE_EXIT`] after writing this stderr text.
    Refuse(String),
}

/// The decision half of `internal-role.ts`, free of process effects so it is testable.
pub fn decide_internal_role(
    env: &std::collections::HashMap<String, String>,
) -> InternalRoleDecision {
    let role = match env.get(TOOL_CHILD_ROLE_ENV) {
        None => return InternalRoleDecision::None,
        Some(role) if role.is_empty() => return InternalRoleDecision::None,
        Some(role) => role.as_str(),
    };
    if role == "launch-shim" {
        return InternalRoleDecision::LaunchShim;
    }
    if role != "tool-child" {
        return InternalRoleDecision::Refuse(format!(
            "senpi: unknown internal role {}\n",
            serde_json::Value::String(role.to_owned())
        ));
    }
    if !channel_env_present(env) {
        return InternalRoleDecision::Refuse("tool-child: channel unavailable\n".to_owned());
    }
    InternalRoleDecision::ToolChild
}

/// Writes the shim's `{"pid":N}` line on descriptor 3 (`writeSync(PID_FD, ...)`). The shim role exists only on
/// Windows (`run_launch_shim_role` exits 70 elsewhere before it would call this), where the tool child hands the shim
/// a pipe as CRT descriptor 3.
///
/// maho-cli inherits `unsafe_code = "forbid"`, so the one CRT write lives in `maho-tool-child` (which allows `unsafe`
/// for its Win32 calls): `launch_shim_role::write_pid_fd`.
#[cfg(windows)]
fn write_pid_fd(line: &str) -> std::io::Result<()> {
    maho_tool_child::launch_shim_role::write_pid_fd(line)
}

#[cfg(not(windows))]
fn write_pid_fd(_line: &str) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "the launch-shim role is Windows only",
    ))
}

/// `internal-role.ts` at module load: returns when this process has no role; otherwise runs the role and never returns.
pub fn run_internal_role() {
    let env = process_env();
    match decide_internal_role(&env) {
        InternalRoleDecision::None => {}
        InternalRoleDecision::LaunchShim => {
            maho_tool_child::launch_shim_role::run_launch_shim_role(&env, write_pid_fd)
        }
        InternalRoleDecision::Refuse(text) => {
            use std::io::Write;
            let _ = std::io::stderr().write_all(text.as_bytes());
            std::process::exit(INTERNAL_ROLE_EXIT);
        }
        // The role module stays out of the CLI graph: a tool child must never load the session, provider or TUI modules.
        InternalRoleDecision::ToolChild => crate::tool_child_role::run_tool_child_role(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use maho_tool_child::channel::{TOOL_CHILD_CHANNEL_ENV, TOOL_CHILD_SECRET_ENV};

    fn env(pairs: &[(&str, &str)]) -> std::collections::HashMap<String, String> {
        pairs
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect()
    }

    #[test]
    fn no_role_or_an_empty_role_runs_the_ordinary_cli() {
        assert_eq!(decide_internal_role(&env(&[])), InternalRoleDecision::None);
        assert_eq!(
            decide_internal_role(&env(&[(TOOL_CHILD_ROLE_ENV, "")])),
            InternalRoleDecision::None
        );
    }

    #[test]
    fn a_tool_child_without_its_channel_exits_70_with_the_channel_message() {
        assert_eq!(
            decide_internal_role(&env(&[(TOOL_CHILD_ROLE_ENV, "tool-child")])),
            InternalRoleDecision::Refuse("tool-child: channel unavailable\n".to_owned())
        );
        assert_eq!(
            decide_internal_role(&env(&[
                (TOOL_CHILD_ROLE_ENV, "tool-child"),
                (TOOL_CHILD_CHANNEL_ENV, "c")
            ])),
            InternalRoleDecision::Refuse("tool-child: channel unavailable\n".to_owned())
        );
        assert_eq!(INTERNAL_ROLE_EXIT, 70);
    }

    #[test]
    fn a_tool_child_with_both_channel_variables_becomes_the_tool_child() {
        assert_eq!(
            decide_internal_role(&env(&[
                (TOOL_CHILD_ROLE_ENV, "tool-child"),
                (TOOL_CHILD_CHANNEL_ENV, "c"),
                (TOOL_CHILD_SECRET_ENV, "s"),
            ])),
            InternalRoleDecision::ToolChild
        );
    }

    #[test]
    fn the_launch_shim_role_is_dispatched_before_the_role_check() {
        assert_eq!(
            decide_internal_role(&env(&[(TOOL_CHILD_ROLE_ENV, "launch-shim")])),
            InternalRoleDecision::LaunchShim
        );
    }

    #[test]
    fn an_unknown_role_is_refused_naming_the_role() {
        assert_eq!(
            decide_internal_role(&env(&[(TOOL_CHILD_ROLE_ENV, "bogus")])),
            InternalRoleDecision::Refuse("senpi: unknown internal role \"bogus\"\n".to_owned())
        );
    }
}
