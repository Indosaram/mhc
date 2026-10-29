use serde_json::Value;

/// Where a command definition was loaded from.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CommandSource {
    #[default]
    ClaudeCode,
    OpenCode,
}

/// Claude Code command models are dropped; OpenCode models are kept when non-blank.
#[must_use]
pub fn sanitize_model_field(model: Option<&Value>, source: CommandSource) -> Option<String> {
    match source {
        CommandSource::ClaudeCode => None,
        CommandSource::OpenCode => model
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|model| !model.is_empty())
            .map(str::to_string),
    }
}
