//! MIT profile (`--no-default-features`): hierarchical project context files from
//! `maho-context-files` (docs/cleanroom/specs/init-deep.md), mounted as
//! `<builtin:context-files>` (`/init-deep [update]` and a once-per-repository suggestion at session
//! start in TUI/RPC/app-server sessions), and `mhc init-deep`, which writes each `AGENTS.md` with a
//! child `mhc -p` run of this executable (read-only tools). `MAHO_CONTEXT_ADVICE=0` or
//! `init_deep.advise: false` turns the suggestion off. The `omo` build does not mount this.

/// The context-files extension.
pub fn extension() -> maho_context_files::ContextFilesExtension {
    maho_context_files::ContextFilesExtension {
        home: Some(std::path::PathBuf::from(maho_core::config::home_dir())),
    }
}

/// Seconds one child model run may take.
pub const CHILD_TIMEOUT_SECS: u64 = 600;

/// `mhc init-deep ...`; returns the exit code.
pub fn run_init_deep_command(args: &[String]) -> Result<i32, String> {
    let cwd = std::env::current_dir().map_err(|error| error.to_string())?;
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    let settings = maho_context_files::Settings::load(&cwd);
    // Offline runs stay offline in the children.
    let offline = std::env::var("PI_OFFLINE").is_ok_and(|v| !v.is_empty() && v != "0");
    let mut make_runner = |model: Option<String>| -> Box<dyn maho_context_files::Runner> {
        Box::new(maho_context_files::ChildRunner {
            executable: executable.clone(),
            model,
            extra_args: if offline {
                vec!["--offline".to_owned()]
            } else {
                Vec::new()
            },
            timeout: std::time::Duration::from_secs(CHILD_TIMEOUT_SECS),
        })
    };
    let output = maho_context_files::cli::run(args, &cwd, settings, &mut make_runner);
    if !output.stdout.is_empty() {
        maho_core::output_guard::maho_write_stdout(&format!("{}\n", output.stdout.trim_end()));
    }
    if !output.stderr.is_empty() {
        maho_core::output_guard::maho_write_stderr(&format!("{}\n", output.stderr.trim_end()));
    }
    Ok(output.code)
}
