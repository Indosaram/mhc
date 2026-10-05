pub fn resolve_cursor_cli_spawn_model(model: &maho_ai::model::Model, selection: Option<&maho_ai::types::ThinkingSelection>) -> String {
    maho_ai::cursor::selection_descriptor::render_cursor_cli_model_string(model,selection)
}
