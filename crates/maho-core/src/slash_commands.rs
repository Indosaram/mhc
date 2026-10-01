//! Port of senpi packages/coding-agent/src/core/slash-commands.ts.

use crate::config::app_name;
use crate::source_info::SourceInfo;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlashCommandSource {
    Extension,
    Prompt,
    Skill,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SlashCommandInfo {
    pub name: String,
    pub description: Option<String>,
    pub source: SlashCommandSource,
    pub source_info: SourceInfo,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltinSlashCommand {
    pub name: &'static str,
    pub description: &'static str,
    pub argument_hint: Option<&'static str>,
}

const fn command(name: &'static str, description: &'static str, argument_hint: Option<&'static str>) -> BuiltinSlashCommand {
    BuiltinSlashCommand { name, description, argument_hint }
}

/// The built-in slash commands, in senpi order. The /quit and /exit descriptions interpolate the
/// product name, so they are appended by builtin_slash_commands().
const BUILTIN_SLASH_COMMANDS: [BuiltinSlashCommand; 24] = [
    command("settings", "Open settings menu", None),
    command("model", "Select model (opens selector UI)", Some("<provider/model>")),
    command("tree", "Navigate session tree (switch branches)", None),
    command("thinking", "Set thinking level", Some("<level>")),
    command("scoped-models", "Enable/disable models for Ctrl+P cycling", None),
    command("favorite-models", "Manage favorite models for Ctrl+P cycling", None),
    command("export", "Export session (HTML default, or specify path: .html/.jsonl)", None),
    command("import", "Import and resume a session from a JSONL file", None),
    command("share", "Share session as a secret GitHub gist", None),
    command("copy", "Copy last agent message to clipboard", None),
    command("rename", "Rename the current session", Some("[name]")),
    command("name", "Alias of /rename (set session display name)", None),
    command("session", "Show session info and stats", None),
    command("changelog", "Show changelog entries", None),
    command("hotkeys", "Show all keyboard shortcuts", None),
    command("fork", "Create a new fork from a previous user message", None),
    command("clone", "Duplicate the current session at the current position", None),
    command("trust", "Save project trust decision for future sessions", None),
    command("login", "Configure provider authentication", Some("<provider>")),
    command("logout", "Remove provider authentication", None),
    command("new", "Start a new session", None),
    command("compact", "Manually compact the session context", None),
    command("resume", "Resume a different session", None),
    command("reload", "Reload keybindings, extensions, skills, prompts, themes, and context files", None),
];

pub fn builtin_slash_commands() -> Vec<BuiltinSlashCommand> {
    let mut commands = BUILTIN_SLASH_COMMANDS.to_vec();
    let quit = format!("Quit {}", app_name());
    let exit = format!("Quit {} (alias of /quit)", app_name());
    commands.push(BuiltinSlashCommand { name: "quit", description: Box::leak(quit.into_boxed_str()), argument_hint: None });
    commands.push(BuiltinSlashCommand { name: "exit", description: Box::leak(exit.into_boxed_str()), argument_hint: None });
    commands
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_builtin_list_keeps_senpi_order_and_names() {
        let commands = builtin_slash_commands();
        assert_eq!(commands.len(), 26);
        assert_eq!(commands[0].name, "settings");
        assert_eq!(commands[1].name, "model");
        assert_eq!(commands[1].argument_hint, Some("<provider/model>"));
        assert_eq!(commands[commands.len() - 2].name, "quit");
        assert_eq!(commands[commands.len() - 1].name, "exit");
    }

    #[test]
    fn the_quit_descriptions_name_the_product() {
        let commands = builtin_slash_commands();
        let quit = commands.iter().find(|command| command.name == "quit").expect("quit");
        assert_eq!(quit.description, "Quit maho");
        let exit = commands.iter().find(|command| command.name == "exit").expect("exit");
        assert_eq!(exit.description, "Quit maho (alias of /quit)");
    }
}
