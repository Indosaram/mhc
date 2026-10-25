pub fn refreshes_session_activity(command: &str) -> bool {
    !matches!(
        command,
        "get_state"
            | "get_effect_status"
            | "get_pending_inputs"
            | "get_messages"
            | "get_entries"
            | "get_tree"
            | "get_fork_messages"
            | "get_last_assistant_text"
            | "get_session_stats"
            | "resolve_path"
            | "get_steering_messages"
            | "get_follow_up_messages"
            | "get_available_models"
            | "get_available_thinking_levels"
            | "get_model_thinking_level"
            | "get_fast_mode"
            | "get_commands"
            | "get_loaded_surfaces"
            | "get_media"
            | "get_auth_providers"
            | "get_provider_accounts"
            | "memory_report"
            | "eval_cells.list"
            | "eval_cells.peek"
    )
}
