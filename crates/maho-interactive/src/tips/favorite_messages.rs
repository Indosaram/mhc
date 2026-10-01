//! Port of senpi `packages/coding-agent/src/modes/interactive/tips/favorite-messages.ts`.

use crate::components::ask_user_answer_key::key_text;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FavoriteCycleStatusKind {
    Empty,
    Single,
}

pub fn build_favorite_cycle_status_message(kind: FavoriteCycleStatusKind) -> String {
    let open_selector = key_text("app.model.select");
    let toggle_favorite = key_text("app.models.toggleFavorite");
    let setup_hint = format!(
        "Press {open_selector} then {toggle_favorite} to favorite models, or run /favorite-models."
    );

    match kind {
        FavoriteCycleStatusKind::Single => {
            format!("Only one favorite model available. {setup_hint}")
        }
        FavoriteCycleStatusKind::Empty => format!(
            "No favorite models configured. {setup_hint} See /help for the full reference."
        ),
    }
}
