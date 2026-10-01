//! Port of senpi `packages/coding-agent/src/modes/interactive/tips/catalog/settings-tips.ts`.

use super::types::TipDefinition;

pub const SETTINGS_TIPS: &[TipDefinition] = &[
    TipDefinition { id: "settings-locations", bindings: &[], requires_command: None, render: |_k| format!("Use /settings for common options; the rest live in settings.json, with {}/settings.json overriding per project.", maho_core::config::config_dir_name()) },
    TipDefinition { id: "permission-preset", bindings: &[], requires_command: None, render: |_k| "Set permissionPreset to \"workspace\", \"read-only\", or \"ask\" to decide which tool calls need your approval.".to_string() },
    TipDefinition { id: "packages-setting", bindings: &[], requires_command: None, render: |_k| "The packages setting loads skills, extensions, prompts, and themes from an npm or git package.".to_string() },
    TipDefinition { id: "custom-themes", bindings: &[], requires_command: None, render: |_k| format!("Switch themes from /settings, and drop custom theme files in {}/themes.", maho_core::config::agent_dir_label()) },
    TipDefinition { id: "reload-resources", bindings: &[], requires_command: None, render: |_k| "Use /reload to re-read keybindings, extensions, skills, prompts, themes, and context files without restarting.".to_string() },
    TipDefinition { id: "project-trust", bindings: &[], requires_command: None, render: |_k| "Use /trust to save a trust decision so this project's settings, skills, and extensions load next time.".to_string() },
    TipDefinition { id: "tips-toggle", bindings: &[], requires_command: None, render: |_k| "Set \"tips\": false in settings.json to stop showing these tips.".to_string() },
];
