//! MIT profile (`--no-default-features`): the subagent delegation tools (`maho-task`): `task`,
//! `task_output`, `task_list`, `task_resume`, `task_cancel` and `/tasks`. Children run as
//! `mhc -p` processes of the current executable; the `omo` build mounts its own task tools.
use std::path::PathBuf;
use std::sync::Arc;

/// Models with usable credentials (`models.json` + `auth.json`, offline), read on a helper thread
/// because the caller is already inside the async runtime.
fn available_models() -> Vec<String> {
    std::thread::spawn(|| super::doctor::available_models().ok().flatten())
        .join()
        .ok()
        .flatten()
        .unwrap_or_default()
}

/// The delegation extension for this process.
pub fn extension() -> maho_task::TaskExtension {
    let runner = maho_task::ProcessRunner::current_exe()
        .unwrap_or_else(|_| maho_task::ProcessRunner::new("mhc"));
    maho_task::TaskExtension {
        agent_dir: Some(PathBuf::from(maho_core::config::get_agent_dir())),
        runner: Arc::new(runner),
        available_models: Arc::new(available_models),
        isolation: None,
        observers: Vec::new(),
        hooks: maho_lsp::hooks::HookChain::default(),
    }
}
