//! MIT profile (`--no-default-features`): the session gateway from `maho-gateway`
//! (docs/cleanroom/specs/gateway.md), mounted as `<builtin:gateway>`. Long-lived sessions (TUI,
//! RPC, app server) register in `<agent dir>/gateway` (or `$MAHO_GATEWAY_DIR`) and receive
//! messages from their inbox as `session_control_delivery` messages; every session gets the
//! `session_list` / `session_send` tools. Also runs `mhc gateway list|send` from the shell.
//! `MAHO_GATEWAY=0` or `gateway.enabled: false` turns it off. The `omo` build does not mount this.

use std::path::PathBuf;

/// The store root of this process.
pub fn store() -> maho_gateway::Store {
    maho_gateway::Store::new(maho_gateway::default_root(
        &PathBuf::from(maho_core::config::get_agent_dir()),
        &|key| std::env::var(key).ok(),
    ))
}

/// The gateway extension; its store root follows each session's agent dir.
pub fn extension() -> maho_gateway::GatewayExtension {
    maho_gateway::GatewayExtension::default()
}

/// `mhc gateway ...`; returns the exit code.
pub fn run_gateway_command(args: &[String]) -> i32 {
    let output = maho_gateway::cli::run(args, &store(), &|key| std::env::var(key).ok());
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
    fn the_store_lives_under_the_agent_dir_by_default() {
        if std::env::var_os(maho_gateway::store::DIR_ENV).is_none() {
            assert!(super::store().root().ends_with("gateway"));
        }
    }
}
