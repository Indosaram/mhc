//! MIT profile (`--no-default-features`): telemetry from `maho-telemetry` (ported from the MIT
//! oh-my-pi telemetry export and stats sources; opt-outs, ledger and `mhc usage` per
//! docs/cleanroom/specs/telemetry.md). Mounts `<builtin:telemetry>`, which appends one line per
//! model call to the local usage ledger (`<agent dir>/stats/usage.jsonl`) and, only when the user
//! configured an `OTEL_EXPORTER_OTLP_*ENDPOINT`, pushes GenAI traces, run metrics and a run log as
//! OTLP/HTTP protobuf. `MAHO_TELEMETRY=0`, `DO_NOT_TRACK=1`, `OTEL_SDK_DISABLED=true`,
//! `MAHO_USAGE_STATS=0` and the `telemetry.*` config keys switch parts off. Also runs
//! `mhc usage ...` from the shell. The `omo` build does not mount this.
use maho_telemetry::{ConfigSwitches, Settings, TelemetryExtension};
use std::path::{Path, PathBuf};

/// The resolved telemetry settings for `cwd` (environment + merged native config).
pub fn settings(cwd: &Path) -> Settings {
    let env = maho_config::process_env();
    let view = maho_config::load_native(cwd, &env).view;
    maho_telemetry::resolve(
        &|key| env.get(key).cloned(),
        ConfigSwitches::from_view(&view),
        &PathBuf::from(maho_core::config::get_agent_dir()),
        env!("CARGO_PKG_VERSION"),
    )
}

/// The telemetry extension for this process.
pub fn extension() -> TelemetryExtension {
    let cwd = std::env::current_dir().unwrap_or_default();
    let mut extension = TelemetryExtension::new(settings(&cwd));
    extension.task_depth = std::env::var(maho_task::settings::DEPTH_ENV).ok();
    extension
}

/// `mhc usage ...`; returns the exit code.
pub fn run_usage_command(args: &[String]) -> Result<i32, String> {
    let cwd = std::env::current_dir().map_err(|error| error.to_string())?;
    let text = maho_telemetry::cli::run(
        args,
        &settings(&cwd),
        Path::new(&maho_core::config::get_sessions_dir()),
    )?;
    maho_core::output_guard::maho_write_stdout(&format!("{}\n", text.trim_end()));
    Ok(0)
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_ledger_lives_under_the_agent_dir_by_default() {
        let settings = super::settings(std::path::Path::new("."));
        if std::env::var_os("MAHO_USAGE_DIR").is_none() {
            assert!(settings.ledger_location.ends_with("stats/usage.jsonl"));
        }
    }
}
