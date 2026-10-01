//! Port of senpi `packages/coding-agent/src/modes/interactive/tips/catalog/model-tips.ts`.

use super::types::TipDefinition;

pub const MODEL_TIPS: &[TipDefinition] = &[
    TipDefinition {
        id: "thinking-level",
        bindings: &["app.thinking.cycle"],
        requires_command: None,
        render: |keys| format!("Use {} to cycle the model's thinking level.", keys("app.thinking.cycle")),
    },
    TipDefinition {
        id: "favorite-model-rotation",
        bindings: &["app.model.cycleForward", "app.model.cycleBackward"],
        requires_command: None,
        render: |keys| {
            format!(
                "Rotate through favorite models with {}; go backward with {}.",
                keys("app.model.cycleForward"),
                keys("app.model.cycleBackward")
            )
        },
    },
    TipDefinition {
        id: "model-selector-favorites",
        bindings: &["app.model.select", "app.models.toggleFavorite"],
        requires_command: None,
        render: |keys| {
            format!(
                "Open the model selector with {}, then use {} to toggle the highlighted favorite.",
                keys("app.model.select"),
                keys("app.models.toggleFavorite")
            )
        },
    },
    TipDefinition {
        id: "model-command-search",
        bindings: &[],
        requires_command: None,
        render: |_keys| "Use /model to open the selector, or /model <pattern> to jump straight to a matching model.".to_string(),
    },
    TipDefinition {
        id: "model-cycling-scope",
        bindings: &["app.model.cycleForward"],
        requires_command: None,
        render: |keys| {
            format!(
                "Start {} with --models \"anthropic/*,gpt-5*\" to limit which models {} cycles through.",
                maho_core::config::app_name(),
                keys("app.model.cycleForward")
            )
        },
    },
    TipDefinition {
        id: "thinking-budgets",
        bindings: &[],
        requires_command: None,
        render: |_keys| "Set thinkingBudgets in settings.json to choose the token budget behind each thinking level.".to_string(),
    },
    TipDefinition {
        id: "prompt-preset",
        bindings: &[],
        requires_command: None,
        render: |_keys| "Set promptPreset in settings.json when a model does not auto-detect the system prompt you want.".to_string(),
    },
    TipDefinition {
        id: "provider-login",
        bindings: &[],
        requires_command: None,
        render: |_keys| "Use /login to add a subscription or API-key provider, and /logout to remove stored credentials.".to_string(),
    },
    TipDefinition {
        id: "fallback-chains-setting",
        bindings: &[],
        requires_command: None,
        render: |_keys| "retry.fallbackChains in settings.json maps a model to ordered fallbacks, so a failed turn retries on the next one.".to_string(),
    },
    TipDefinition {
        id: "fallback-command",
        bindings: &[],
        requires_command: Some("fallback"),
        render: |_keys| "Use /fallback to inspect and manage the retry fallback chain for the current model.".to_string(),
    },
];
