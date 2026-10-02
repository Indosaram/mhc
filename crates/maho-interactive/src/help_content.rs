use maho_core::{keybindings::keybindings, slash_commands::{SlashCommandInfo, builtin_slash_commands}};
use crate::components::keybinding_hints::key_display_text;

fn escape_table_cell(value: &str) -> String { value.replace('|', "\\|").replace('\n', " ") }

pub fn build_help_markdown(extension_commands: &[SlashCommandInfo]) -> String {
    let groups = [("tui.editor.", "Editor"), ("tui.input.", "Input"), ("tui.select.", "Selection"), ("tui.altScreen.", "Alt Screen"), ("app.", "Application")];
    let started = [
        format!("- Press `{}` to submit; use `{}` to add a new line.", key_display_text("tui.input.submit"), key_display_text("tui.input.newLine")),
        "- Type `!` to run bash, or `!!` to run bash without adding the command or output to context.".into(),
        "- Type `/` for commands.".into(),
        "- Drop files into the terminal to attach them.".into(),
        format!("- Press `{}` to paste an image, with text fallback.", key_display_text("app.clipboard.pasteImage")),
        format!("- Press `{}` to queue a follow-up message.", key_display_text("app.message.followUp")),
        "- Press `?` on an empty input to show the shortcut overlay.".into(),
    ].join("\n");
    let tables = groups.map(|(prefix, heading)| {
        let mut lines = vec![format!("### {heading}"), "| Key | Action |".into(), "|-----|--------|".into()];
        for (id, definition) in keybindings() {
            if !id.starts_with(prefix) { continue; }
            let keys = key_display_text(id);
            if keys.is_empty() { continue; }
            lines.push(format!("| `{}` | {} |", escape_table_cell(&keys), escape_table_cell(definition.description.as_deref().unwrap_or_default())));
        }
        lines.join("\n")
    }).join("\n\n");
    let mut names = std::collections::HashSet::new();
    let mut commands = Vec::new();
    for command in builtin_slash_commands() {
        names.insert(command.name.to_owned());
        let description = if command.name == "favorite-models" { format!("Manage favorite models for {} cycling", key_display_text("app.model.cycleForward")) } else { command.description.to_owned() };
        commands.push(format!("/{} — {description}", command.name));
    }
    for command in extension_commands {
        if names.insert(command.name.clone()) { commands.push(format!("/{} — {}", command.name, command.description.as_deref().unwrap_or_default())); }
    }
    ["## Getting started", &started, "## Keybindings", &tables, "## Commands", &commands.join("\n"), "## Account display names",
        "Use `/gpt-account rename <id> <display name...>`, `/claude-account rename <id> <display name...>`, or `/account <provider> rename <id> <display name...>`. Use `clear-name <id>` on the same command to remove the label.",
        "Labels display as `displayName (id)`; pins, removal, refresh and session affinity always use the unchanged ID. Labels are Unicode-normalized (NFC) with internal whitespace collapsed, limited to 32 terminal columns, must contain at least one visible character, and cannot contain control or formatting characters. Two labels that render identically cannot coexist in one provider: comparison folds case, compatibility forms, invisible code points, and Cyrillic lookalikes. Environment accounts cannot be renamed.",
        "`/gpt-account add` offers an optional display name after saving the account, when Senpi generated the account ID itself. The Claude lane asks for the account name through its own login prompt, so it never asks twice. Leave it blank or cancel to keep the successful login without a label. Legacy accounts display only their ID."
    ].join("\n\n")
}
