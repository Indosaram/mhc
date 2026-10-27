//! MIT profile (`--no-default-features`): the subagent delegation tools (`maho-task`): `task`,
//! `task_output`, `task_list`, `task_resume`, `task_cancel` and `/tasks`. Children run as
//! `mhc -p` processes of the current executable; the `omo` build mounts its own task tools.
//! Isolated tasks (`isolated: true` or `task.isolation.enabled`) run in a git worktree (or a copy
//! outside git) under `$MAHO_WORKTREE_DIR` or `<agent dir>/wt` (`maho-isolation`).
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

/// The isolation backend: isolations live under `$MAHO_WORKTREE_DIR` or `<agent dir>/wt`.
/// Isolations left behind by processes that are gone are swept on a helper thread.
pub fn isolation(agent_dir: &std::path::Path) -> maho_isolation::TaskIsolation {
    let isolation = maho_isolation::TaskIsolation::new(maho_isolation::base_dir(Some(agent_dir)));
    let sweeper = isolation.clone();
    let _ = std::thread::Builder::new()
        .name("isolation-sweep".into())
        .spawn(move || sweeper.clear_stale(false));
    isolation
}

/// The delegation extension for this process.
pub fn extension() -> maho_task::TaskExtension {
    let agent_dir = PathBuf::from(maho_core::config::get_agent_dir());
    let runner = maho_task::ProcessRunner::current_exe()
        .unwrap_or_else(|_| maho_task::ProcessRunner::new("mhc"));
    maho_task::TaskExtension {
        isolation: Some(Arc::new(isolation(&agent_dir))),
        agent_dir: Some(agent_dir),
        runner: Arc::new(runner),
        available_models: Arc::new(available_models),
        observers: Vec::new(),
        hooks: maho_lsp::hooks::HookChain::default(),
    }
}
