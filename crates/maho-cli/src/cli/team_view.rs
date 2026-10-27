//! MIT profile (`--no-default-features`): team runs and task panes from `maho-team`, attached to
//! `<builtin:task>`: the `team_create`, `team_members`, `team_message`, `team_wait` and
//! `team_stop` tools and `/team`, plus a task observer that follows delegated tasks in tmux/cmux
//! panes (`MAHO_TEAM_PANES=auto|all|off`, `MAHO_TEAM_MUX`, `MAHO_TEAM_PANE_LAYOUT`,
//! `MAHO_TEAM_PANE_LINGER`) and stays headless outside a multiplexer. The `omo` build does not
//! mount them.
use std::sync::Arc;

/// The pane observer for this process (backend detected from the environment).
pub fn pane_observer() -> Arc<maho_team::PaneObserver> {
    Arc::new(maho_team::PaneObserver::from_process_env())
}

/// The team tools, reporting panes of `panes`.
pub fn addon(panes: Arc<maho_team::PaneObserver>) -> maho_team::TeamAddon {
    maho_team::TeamAddon { panes: Some(panes) }
}

#[cfg(test)]
mod tests {
    use maho_task::TaskAddon;

    #[test]
    fn registers_the_team_tools() {
        let panes = super::pane_observer();
        let addon = super::addon(panes);
        let names: Vec<String> = addon.tools().into_iter().map(|tool| tool.name).collect();
        assert_eq!(names, maho_team::TOOL_NAMES);
        assert_eq!(addon.commands()[0].name, "team");
    }
}
