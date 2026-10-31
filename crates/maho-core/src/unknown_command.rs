//! Port of senpi `packages/coding-agent/src/core/unknown-command.ts`.

use std::collections::BTreeSet;
use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

/// A leading `/name` token; a second `/` (`/tmp/a.txt`) makes the token a path, not a command.
static COMMAND_TOKEN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^/[A-Za-z][A-Za-z0-9:_-]*$").unwrap_or_else(|e| panic!("command token: {e}"))
});
const MAX_SUGGESTIONS: usize = 3;

/// `unknown`: nothing registers the name. `interactive_only`: a TUI builtin sent as a prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnknownCommandReason {
    Unknown,
    InteractiveOnly,
}

impl UnknownCommandReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::InteractiveOnly => "interactive_only",
        }
    }
}

/// Names a prompt can resolve, split by whether the prompt path itself can run them.
#[derive(Debug, Clone, Default)]
pub struct CommandCatalog {
    /// Extension commands, prompt templates, and `skill:<name>` for loaded skills.
    pub prompt_commands: BTreeSet<String>,
    /// TUI builtins (`model`, `settings`, ...) that only the interactive editor dispatches.
    pub interactive_commands: BTreeSet<String>,
}

/// Command-shaped input that no command handles, rejected before it reaches the model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownCommandError {
    /// The command name without its leading `/`.
    pub command: String,
    /// Up to three known command names (without `/`) close to `command`.
    pub suggestions: Vec<String>,
    pub reason: UnknownCommandReason,
}

impl UnknownCommandError {
    pub const NAME: &'static str = "UnknownCommandError";

    pub fn new(
        command: impl Into<String>,
        suggestions: Vec<String>,
        reason: UnknownCommandReason,
    ) -> Self {
        Self {
            command: command.into(),
            suggestions,
            reason,
        }
    }
}

impl std::fmt::Display for UnknownCommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&format_unknown_command_message(
            &self.command,
            &self.suggestions,
            self.reason,
        ))
    }
}

impl std::error::Error for UnknownCommandError {}

fn format_unknown_command_message(
    command: &str,
    suggestions: &[String],
    reason: UnknownCommandReason,
) -> String {
    if reason == UnknownCommandReason::InteractiveOnly {
        return format!("/{command} is an interactive command and cannot be sent as a prompt.");
    }
    let hint = if suggestions.is_empty() {
        String::new()
    } else {
        format!(
            " Did you mean {}?",
            suggestions
                .iter()
                .map(|name| format!("/{name}"))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    format!("Unknown command /{command}.{hint}")
}

/// How a protocol client confirms that a refused unknown command should be sent as text.
pub const UNKNOWN_COMMAND_CONFIRM_HINT: &str =
    "Resend with \"unknownCommandAsText\": true to send it as text.";

/// Rebuild the typed rejection from an RPC `unknown_command` failure's `errorData`, if it is well formed.
pub fn unknown_command_error_from_wire(data: &Value) -> Option<UnknownCommandError> {
    let data = data.as_object()?;
    let command = data.get("command")?.as_str()?;
    let suggestions = data.get("suggestions")?.as_array()?;
    let reason = match data.get("reason")?.as_str()? {
        "unknown" => UnknownCommandReason::Unknown,
        "interactive_only" => UnknownCommandReason::InteractiveOnly,
        _ => return None,
    };
    let names = suggestions
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect();
    Some(UnknownCommandError::new(command, names, reason))
}

/// The command name of command-shaped text, or `None` when the text is ordinary prose or a path.
pub fn command_shaped_name(text: &str) -> Option<String> {
    let token = text
        .trim()
        .split(char::is_whitespace)
        .next()
        .unwrap_or_default();
    COMMAND_TOKEN.is_match(token).then(|| token[1..].to_owned())
}

/// The rejection for command-shaped `text` whose name the catalog cannot resolve, if any.
pub fn find_unknown_command(text: &str, catalog: &CommandCatalog) -> Option<UnknownCommandError> {
    let command = command_shaped_name(text)?;
    if catalog.prompt_commands.contains(&command) {
        return None;
    }
    if catalog.interactive_commands.contains(&command) {
        return Some(UnknownCommandError::new(
            command,
            Vec::new(),
            UnknownCommandReason::InteractiveOnly,
        ));
    }
    let mut known: Vec<String> = Vec::new();
    for name in catalog
        .prompt_commands
        .iter()
        .chain(catalog.interactive_commands.iter())
    {
        if !known.contains(name) {
            known.push(name.clone());
        }
    }
    let suggestions = maho_tui::fuzzy::fuzzy_filter(&known, &command, Clone::clone)
        .into_iter()
        .take(MAX_SUGGESTIONS)
        .collect();
    Some(UnknownCommandError::new(
        command,
        suggestions,
        UnknownCommandReason::Unknown,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn catalog() -> CommandCatalog {
        CommandCatalog {
            prompt_commands: ["review", "skill:deploy"].map(str::to_owned).into(),
            interactive_commands: ["model", "settings"].map(str::to_owned).into(),
        }
    }

    #[test]
    fn command_shape_excludes_paths_and_prose() {
        assert_eq!(
            command_shaped_name("  /review now").as_deref(),
            Some("review")
        );
        assert_eq!(
            command_shaped_name("/skill:deploy").as_deref(),
            Some("skill:deploy")
        );
        assert_eq!(command_shaped_name("/tmp/a.txt"), None);
        assert_eq!(command_shaped_name("hello /review"), None);
        assert_eq!(command_shaped_name("/1abc"), None);
    }

    #[test]
    fn known_prompt_commands_pass_and_interactive_ones_are_refused() {
        assert_eq!(find_unknown_command("/review", &catalog()), None);
        let refused = find_unknown_command("/model", &catalog());
        assert_eq!(
            refused.map(|error| error.to_string()).as_deref(),
            Some("/model is an interactive command and cannot be sent as a prompt.")
        );
    }

    #[test]
    fn unknown_commands_suggest_close_names() {
        let error = find_unknown_command("/revew", &catalog());
        assert!(
            error
                .as_ref()
                .is_some_and(|error| error.reason == UnknownCommandReason::Unknown)
        );
        assert!(
            error
                .as_ref()
                .is_some_and(|error| error.suggestions.len() <= MAX_SUGGESTIONS)
        );
        assert_eq!(
            UnknownCommandError::new(
                "x",
                vec!["a".into(), "b".into()],
                UnknownCommandReason::Unknown
            )
            .to_string(),
            "Unknown command /x. Did you mean /a, /b?"
        );
        assert_eq!(
            UnknownCommandError::new("x", Vec::new(), UnknownCommandReason::Unknown).to_string(),
            "Unknown command /x."
        );
    }

    #[test]
    fn wire_data_round_trips_only_when_well_formed() {
        let error = unknown_command_error_from_wire(
            &json!({"command":"x","suggestions":["a",1,"b"],"reason":"unknown"}),
        );
        assert_eq!(
            error.map(|error| error.suggestions),
            Some(vec!["a".to_owned(), "b".to_owned()])
        );
        assert!(
            unknown_command_error_from_wire(
                &json!({"command":"x","suggestions":[],"reason":"other"})
            )
            .is_none()
        );
        assert!(
            unknown_command_error_from_wire(
                &json!({"command":1,"suggestions":[],"reason":"unknown"})
            )
            .is_none()
        );
        assert!(unknown_command_error_from_wire(&json!(null)).is_none());
    }
}
