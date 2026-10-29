//! MIT profile (`--no-default-features`): `mhc home` from `maho-agent-home`
//! (docs/cleanroom/specs/agent-home.md) — the agent home layout over the senpi agent dir, its
//! bootstrap and the `auth.json` privacy check. The `omo` build does not use this.

use std::path::PathBuf;

/// The agent home of this process (`$HOME/.omo` and the resolved agent dir).
pub fn layout() -> maho_agent_home::AgentHome {
    maho_agent_home::AgentHome::new(
        &PathBuf::from(maho_core::config::home_dir()),
        &PathBuf::from(maho_core::config::get_agent_dir()),
    )
}

/// `mhc home ...`; returns the exit code.
pub fn run_home_command(args: &[String]) -> i32 {
    let output = maho_agent_home::cli::run(args, &layout());
    if !output.stdout.is_empty() {
        maho_core::output_guard::maho_write_stdout(&format!("{}\n", output.stdout.trim_end()));
    }
    if !output.stderr.is_empty() {
        maho_core::output_guard::maho_write_stderr(&format!("{}\n", output.stderr.trim_end()));
    }
    output.code
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_layout_uses_the_resolved_agent_dir() {
        let home = super::layout();
        assert_eq!(
            home.agent_dir(),
            std::path::Path::new(&maho_core::config::get_agent_dir())
        );
        assert_eq!(home.root().file_name(), Some(std::ffi::OsStr::new(".omo")));
    }
}
