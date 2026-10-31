//! Port of senpi `packages/coding-agent/src/cli/config-command-help.ts` (v2026.10.27-2): the
//! `config` help moved out of package-manager-cli and gained the `config agent-dir --json` section.

pub const CONFIG_COMMAND_USAGE: &str = "mhc config [-l] [--approve|--no-approve]";

pub fn config_command_help() -> &'static str {
    "Usage:\n  mhc config [-l] [--approve|--no-approve]\n  mhc config import-pi [auth.json|keybindings.json|models.json|settings.json ...]\n\nOpen the resource configuration TUI to enable or disable package resources.\nWithout -l, starts in global settings (~/.omo/agent/settings.json).\nPress Tab in the TUI to switch between global and project-local modes.\n\nimport-pi copies selected Pi config files, or all changed files when none are named.\nExisting destination files are backed up before import.\n\nOptions:\n  -l, --local       Edit project overrides (.omo/settings.json)\n  -a, --approve     Trust project-local files for this command with -l\n  -na, --no-approve Ignore project-local files for this command with -l\n\n  mhc config agent-dir --json\n\nRead the session engine's agent-directory selection without opening settings or starting a host.\nEvery outcome is one versioned JSON record: exit 0 for success, 2 for invalid arguments,\nor 1 when the directory cannot be resolved.\n\n"
}

#[cfg(test)]
mod tests {
    #[test]
    fn help_names_the_agent_dir_command_and_its_exit_codes() {
        let help = super::config_command_help();
        assert!(help.contains("config agent-dir --json"));
        assert!(help.contains("exit 0 for success, 2 for invalid arguments"));
        assert!(help.contains("config import-pi"));
    }
}
