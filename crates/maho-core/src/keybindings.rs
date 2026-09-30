//! Port of senpi `packages/coding-agent/src/core/keybindings.ts` (the core `app.*` ids; the
//! `tui.*` table itself lives in maho-tui, todo 6).

use std::collections::HashMap;
use std::path::Path;
use std::sync::LazyLock;

use maho_tui::keybindings::{
    KeybindingDefinition, KeybindingDefinitions, KeybindingsConfig, Keys, tui_keybindings,
};
use maho_tui::keys::KeyId;
use serde_json::{Map, Value};

use crate::config::get_agent_dir;
use crate::text::strip_bom;

/// `AppKeybindings`: the core application keybinding ids, in declaration order.
pub const APP_KEYBINDING_IDS: [&str; 48] = [
    "app.interrupt",
    "app.clear",
    "app.exit",
    "app.suspend",
    "app.thinking.cycle",
    "app.thinking.save",
    "app.model.cycleForward",
    "app.model.cycleBackward",
    "app.model.select",
    "app.history.search",
    "app.tools.expand",
    "app.thinking.toggle",
    "app.session.toggleNamedFilter",
    "app.editor.external",
    "app.message.copy",
    "app.message.followUp",
    "app.message.dequeue",
    "app.question.answer",
    "app.question.next",
    "app.clipboard.pasteImage",
    "app.session.new",
    "app.session.tree",
    "app.session.fork",
    "app.session.resume",
    "app.session.renameCurrent",
    "app.tree.foldOrUp",
    "app.tree.unfoldOrDown",
    "app.tree.editLabel",
    "app.tree.editMessage",
    "app.tree.toggleLabelTimestamp",
    "app.session.togglePath",
    "app.session.toggleSort",
    "app.session.rename",
    "app.session.delete",
    "app.session.deleteNoninvasive",
    "app.models.save",
    "app.models.toggleFavorite",
    "app.models.enableAll",
    "app.models.clearAll",
    "app.models.toggleProvider",
    "app.models.reorderUp",
    "app.models.reorderDown",
    "app.tree.filter.default",
    "app.tree.filter.noTools",
    "app.tree.filter.userOnly",
    "app.tree.filter.labeledOnly",
    "app.tree.filter.all",
    "app.tree.filter.cycleForward",
];

/// `QUESTION_ANSWER_PRIMARY_KEY`.
pub const QUESTION_ANSWER_PRIMARY_KEY: &str = "alt+up";
/// `QUESTION_ANSWER_FALLBACK_KEY`.
pub const QUESTION_ANSWER_FALLBACK_KEY: &str = "alt+a";

/// `process.platform` spelled the way Node spells it.
pub fn host_platform() -> String {
    match std::env::consts::OS {
        "macos" => "darwin".to_string(),
        "windows" => "win32".to_string(),
        other => other.to_string(),
    }
}

/// `useWindowsKeybindings(platform, env)`.
pub fn use_windows_keybindings(platform: &str, env: &HashMap<String, String>) -> bool {
    platform == "win32"
        || (platform == "linux"
            && (env.get("WSL_DISTRO_NAME").is_some_and(|value| !value.is_empty())
                || env.get("WSL_INTEROP").is_some_and(|value| !value.is_empty())))
}

/// `KEYBINDINGS`: the TUI table plus the core overrides and `app.*` entries.
pub fn keybindings_for(platform: &str, windows: bool) -> KeybindingDefinitions {
    let mut definitions = tui_keybindings();

    let undo_keys: Keys = if platform == "win32" {
        "ctrl+z".into()
    } else if windows {
        "alt+z".into()
    } else {
        "ctrl+-".into()
    };
    override_defaults(&mut definitions, "tui.editor.undo", undo_keys);
    override_defaults(
        &mut definitions,
        "tui.altScreen.previousPrompt",
        if windows { "ctrl+up".into() } else { ["ctrl+shift+up", "ctrl+up"].into() },
    );
    override_defaults(
        &mut definitions,
        "tui.altScreen.nextPrompt",
        if windows { "ctrl+down".into() } else { ["ctrl+shift+down", "ctrl+down"].into() },
    );
    override_defaults(
        &mut definitions,
        "tui.altScreen.search",
        if windows { "ctrl+f".into() } else { "ctrl+shift+f".into() },
    );

    let suspend_keys: Keys = if platform == "win32" { Keys::Many(Vec::new()) } else { "ctrl+z".into() };
    let fold_or_up: Keys = if platform == "darwin" {
        ["alt+left", "ctrl+left"].into()
    } else {
        ["ctrl+left", "alt+left"].into()
    };
    let unfold_or_down: Keys = if platform == "darwin" {
        ["alt+right", "ctrl+right"].into()
    } else {
        ["ctrl+right", "alt+right"].into()
    };
    let empty: Keys = Keys::Many(Vec::new());

    let app_entries: Vec<(&str, Keys, &str)> = vec![
        ("app.interrupt", "escape".into(), "Cancel or abort"),
        ("app.clear", "ctrl+c".into(), "Clear editor"),
        ("app.exit", "ctrl+d".into(), "Exit when editor is empty"),
        ("app.suspend", suspend_keys, "Suspend to background"),
        ("app.thinking.cycle", "shift+tab".into(), "Cycle thinking level"),
        ("app.thinking.save", "ctrl+s".into(), "Save thinking level"),
        ("app.model.cycleForward", "ctrl+p".into(), "Cycle to next model"),
        (
            "app.model.cycleBackward",
            if windows { "alt+p".into() } else { "shift+ctrl+p".into() },
            "Cycle to previous model",
        ),
        ("app.model.select", "ctrl+l".into(), "Open model selector"),
        ("app.history.search", "ctrl+r".into(), "Search prompt history across sessions"),
        ("app.tools.expand", "ctrl+o".into(), "Toggle tool output"),
        ("app.thinking.toggle", "ctrl+t".into(), "Toggle thinking blocks"),
        ("app.session.toggleNamedFilter", "ctrl+n".into(), "Toggle named session filter"),
        ("app.editor.external", "ctrl+g".into(), "Open external editor"),
        ("app.message.copy", "ctrl+x".into(), "Copy message to clipboard"),
        (
            "app.message.followUp",
            if windows { "ctrl+q".into() } else { "alt+enter".into() },
            "Queue follow-up message",
        ),
        (
            "app.message.dequeue",
            if windows { "alt+q".into() } else { "alt+up".into() },
            "Restore queued messages",
        ),
        (
            "app.question.answer",
            [QUESTION_ANSWER_PRIMARY_KEY, QUESTION_ANSWER_FALLBACK_KEY].into(),
            "Open the pending question",
        ),
        ("app.question.next", "alt+down".into(), "Show the next pending question"),
        (
            "app.clipboard.pasteImage",
            if windows { "alt+v".into() } else { "ctrl+v".into() },
            "Paste image from clipboard (text fallback)",
        ),
        ("app.session.new", empty.clone(), "Start a new session"),
        ("app.session.tree", empty.clone(), "Open session tree"),
        ("app.session.fork", empty.clone(), "Fork current session"),
        ("app.session.resume", empty.clone(), "Resume a session"),
        ("app.session.renameCurrent", empty.clone(), "Rename the current session"),
        ("app.tree.foldOrUp", fold_or_up, "Fold tree branch or move up"),
        ("app.tree.unfoldOrDown", unfold_or_down, "Unfold tree branch or move down"),
        ("app.tree.editLabel", "shift+l".into(), "Edit tree label"),
        (
            "app.tree.editMessage",
            "ctrl+e".into(),
            "Edit the selected assistant response, or reopen a user message in the editor",
        ),
        ("app.tree.toggleLabelTimestamp", "shift+t".into(), "Toggle tree label timestamps"),
        ("app.session.togglePath", "ctrl+p".into(), "Toggle session path display"),
        ("app.session.toggleSort", "ctrl+s".into(), "Toggle session sort mode"),
        ("app.session.rename", "ctrl+r".into(), "Rename session"),
        ("app.session.delete", "ctrl+d".into(), "Delete session"),
        ("app.session.deleteNoninvasive", "ctrl+backspace".into(), "Delete session when query is empty"),
        ("app.models.save", "ctrl+s".into(), "Save model selection"),
        ("app.models.toggleFavorite", "ctrl+f".into(), "Toggle favorite model"),
        ("app.models.enableAll", "ctrl+a".into(), "Enable all models"),
        ("app.models.clearAll", "ctrl+x".into(), "Clear all models"),
        ("app.models.toggleProvider", "ctrl+p".into(), "Toggle all models for provider"),
        ("app.models.reorderUp", "alt+up".into(), "Move model up in order"),
        ("app.models.reorderDown", "alt+down".into(), "Move model down in order"),
        ("app.tree.filter.default", "ctrl+d".into(), "Tree filter: default view"),
        ("app.tree.filter.noTools", "ctrl+t".into(), "Tree filter: hide tool results"),
        ("app.tree.filter.userOnly", "ctrl+u".into(), "Tree filter: user messages only"),
        ("app.tree.filter.labeledOnly", "ctrl+l".into(), "Tree filter: labeled entries only"),
        ("app.tree.filter.all", "ctrl+a".into(), "Tree filter: show all entries"),
        ("app.tree.filter.cycleForward", "ctrl+o".into(), "Tree filter: cycle forward"),
        ("app.tree.filter.cycleBackward", "shift+ctrl+o".into(), "Tree filter: cycle backward"),
    ];

    for (id, default_keys, description) in app_entries {
        definitions.insert(
            id.to_string(),
            KeybindingDefinition { default_keys, description: Some(description.to_string()) },
        );
    }

    definitions
}

fn override_defaults(definitions: &mut KeybindingDefinitions, id: &str, default_keys: Keys) {
    if let Some(definition) = definitions.get_mut(id) {
        definition.default_keys = default_keys;
    }
}

/// `KEYBINDINGS` built for this host, once.
pub fn keybindings() -> &'static KeybindingDefinitions {
    static KEYBINDINGS: LazyLock<KeybindingDefinitions> =
        LazyLock::new(|| keybindings_for(&host_platform(), use_windows_keybindings(&host_platform(), &current_env())));
    &KEYBINDINGS
}

fn current_env() -> HashMap<String, String> {
    std::env::vars().collect()
}

/// `KEYBINDING_NAME_MIGRATIONS`: legacy flat names to their namespaced ids, in order.
pub const KEYBINDING_NAME_MIGRATIONS: [(&str, &str); 60] = [
    ("cursorUp", "tui.editor.cursorUp"),
    ("cursorDown", "tui.editor.cursorDown"),
    ("cursorLeft", "tui.editor.cursorLeft"),
    ("cursorRight", "tui.editor.cursorRight"),
    ("cursorWordLeft", "tui.editor.cursorWordLeft"),
    ("cursorWordRight", "tui.editor.cursorWordRight"),
    ("cursorLineStart", "tui.editor.cursorLineStart"),
    ("cursorLineEnd", "tui.editor.cursorLineEnd"),
    ("jumpForward", "tui.editor.jumpForward"),
    ("jumpBackward", "tui.editor.jumpBackward"),
    ("pageUp", "tui.editor.pageUp"),
    ("pageDown", "tui.editor.pageDown"),
    ("deleteCharBackward", "tui.editor.deleteCharBackward"),
    ("deleteCharForward", "tui.editor.deleteCharForward"),
    ("deleteWordBackward", "tui.editor.deleteWordBackward"),
    ("deleteWordForward", "tui.editor.deleteWordForward"),
    ("deleteToLineStart", "tui.editor.deleteToLineStart"),
    ("deleteToLineEnd", "tui.editor.deleteToLineEnd"),
    ("yank", "tui.editor.yank"),
    ("yankPop", "tui.editor.yankPop"),
    ("undo", "tui.editor.undo"),
    ("newLine", "tui.input.newLine"),
    ("submit", "tui.input.submit"),
    ("tab", "tui.input.tab"),
    ("copy", "tui.input.copy"),
    ("selectUp", "tui.select.up"),
    ("selectDown", "tui.select.down"),
    ("selectPageUp", "tui.select.pageUp"),
    ("selectPageDown", "tui.select.pageDown"),
    ("selectConfirm", "tui.select.confirm"),
    ("selectCancel", "tui.select.cancel"),
    ("interrupt", "app.interrupt"),
    ("clear", "app.clear"),
    ("exit", "app.exit"),
    ("suspend", "app.suspend"),
    ("cycleThinkingLevel", "app.thinking.cycle"),
    ("cycleModelForward", "app.model.cycleForward"),
    ("cycleModelBackward", "app.model.cycleBackward"),
    ("selectModel", "app.model.select"),
    ("expandTools", "app.tools.expand"),
    ("toggleThinking", "app.thinking.toggle"),
    ("toggleSessionNamedFilter", "app.session.toggleNamedFilter"),
    ("externalEditor", "app.editor.external"),
    ("followUp", "app.message.followUp"),
    ("dequeue", "app.message.dequeue"),
    ("pasteImage", "app.clipboard.pasteImage"),
    ("newSession", "app.session.new"),
    ("tree", "app.session.tree"),
    ("fork", "app.session.fork"),
    ("resume", "app.session.resume"),
    ("treeFoldOrUp", "app.tree.foldOrUp"),
    ("treeUnfoldOrDown", "app.tree.unfoldOrDown"),
    ("treeEditLabel", "app.tree.editLabel"),
    ("treeEditMessage", "app.tree.editMessage"),
    ("treeToggleLabelTimestamp", "app.tree.toggleLabelTimestamp"),
    ("toggleSessionPath", "app.session.togglePath"),
    ("toggleSessionSort", "app.session.toggleSort"),
    ("renameSession", "app.session.rename"),
    ("deleteSession", "app.session.delete"),
    ("deleteSessionNoninvasive", "app.session.deleteNoninvasive"),
];

fn is_legacy_keybinding_name(key: &str) -> Option<&'static str> {
    KEYBINDING_NAME_MIGRATIONS.iter().find(|(legacy, _)| *legacy == key).map(|(_, next)| *next)
}

/// `migrateKeybindingsConfig`.
#[derive(Debug, Clone, PartialEq)]
pub struct MigrateKeybindingsResult {
    pub config: Map<String, Value>,
    pub migrated: bool,
}

pub fn migrate_keybindings_config(raw_config: &Map<String, Value>) -> MigrateKeybindingsResult {
    let mut config: Map<String, Value> = Map::new();
    let mut migrated = false;

    for (key, value) in raw_config {
        let next_key = is_legacy_keybinding_name(key).unwrap_or(key.as_str());
        if next_key != key {
            migrated = true;
        }
        if key != next_key && raw_config.contains_key(next_key) {
            migrated = true;
            continue;
        }
        config.insert(next_key.to_string(), value.clone());
    }

    MigrateKeybindingsResult { config: order_keybindings_config(&config), migrated }
}

/// `orderKeybindingsConfig`: known ids in table order, then the remaining keys sorted.
pub fn order_keybindings_config(config: &Map<String, Value>) -> Map<String, Value> {
    let mut ordered: Map<String, Value> = Map::new();
    for keybinding in keybindings().keys() {
        if let Some(value) = config.get(keybinding) {
            ordered.insert(keybinding.clone(), value.clone());
        }
    }

    let mut extras: Vec<&String> =
        config.keys().filter(|key| !ordered.contains_key(*key)).collect();
    extras.sort();
    for key in extras {
        if let Some(value) = config.get(key) {
            ordered.insert(key.clone(), value.clone());
        }
    }

    ordered
}

/// `loadRawConfig`: a missing or malformed file yields `None`.
pub fn load_raw_config(path: &str) -> Option<Map<String, Value>> {
    let text = std::fs::read_to_string(path).ok()?;
    let parsed: Value = serde_json::from_str(strip_bom(&text)).ok()?;
    match parsed {
        Value::Object(map) => Some(map),
        _ => None,
    }
}

/// `toKeybindingsConfig`: strings and string arrays survive, anything else is dropped.
pub fn to_keybindings_config(value: &Map<String, Value>) -> KeybindingsConfig {
    let mut config: KeybindingsConfig = KeybindingsConfig::new();
    for (key, binding) in value {
        match binding {
            Value::String(one) => {
                config.insert(key.clone(), Some(Keys::One(one.clone())));
            }
            Value::Array(entries) if entries.iter().all(Value::is_string) => {
                let keys: Vec<KeyId> = entries
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect();
                config.insert(key.clone(), Some(Keys::Many(keys)));
            }
            _ => {}
        }
    }
    config
}

/// `KeybindingsManager`.
#[derive(Debug, Clone)]
pub struct KeybindingsManager {
    inner: maho_tui::keybindings::KeybindingsManager,
    config_path: Option<String>,
}

impl KeybindingsManager {
    pub fn new(user_bindings: KeybindingsConfig, config_path: Option<String>) -> Self {
        Self {
            inner: maho_tui::keybindings::KeybindingsManager::new(keybindings().clone(), user_bindings),
            config_path,
        }
    }

    /// `KeybindingsManager.create(agentDir)`.
    pub fn create(agent_dir: Option<&str>) -> Self {
        let agent_dir = agent_dir.map(str::to_string).unwrap_or_else(get_agent_dir);
        let config_path = Path::new(&agent_dir).join("keybindings.json").to_string_lossy().into_owned();
        let user_bindings = Self::load_from_file(&config_path);
        Self::new(user_bindings, Some(config_path))
    }

    /// `reload`.
    pub fn reload(&mut self) {
        let Some(config_path) = self.config_path.clone() else {
            return;
        };
        self.inner.set_user_bindings(Self::load_from_file(&config_path));
    }

    /// `getEffectiveConfig`.
    pub fn get_effective_config(&self) -> KeybindingsConfig {
        self.inner.get_resolved_bindings()
    }

    /// `loadFromFile`.
    pub fn load_from_file(path: &str) -> KeybindingsConfig {
        let Some(raw_config) = load_raw_config(path) else {
            return KeybindingsConfig::new();
        };
        to_keybindings_config(&migrate_keybindings_config(&raw_config).config)
    }

    pub fn inner(&self) -> &maho_tui::keybindings::KeybindingsManager {
        &self.inner
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn raw(entries: &[(&str, Value)]) -> Map<String, Value> {
        let mut map = Map::new();
        for (key, value) in entries {
            map.insert((*key).to_string(), value.clone());
        }
        map
    }

    #[test]
    fn app_ids_are_all_present_with_their_defaults() {
        let definitions = keybindings();
        assert_eq!(definitions["app.interrupt"].default_keys, Keys::One("escape".to_string()));
        assert_eq!(definitions["app.message.followUp"].default_keys, Keys::One("alt+enter".to_string()));
        assert_eq!(
            definitions["app.question.answer"].default_keys,
            Keys::Many(vec!["alt+up".to_string(), "alt+a".to_string()])
        );
        assert_eq!(definitions["app.session.new"].default_keys, Keys::Many(Vec::new()));
        for id in APP_KEYBINDING_IDS {
            assert!(definitions.contains_key(id), "{id} is defined");
        }
        assert!(definitions.contains_key("tui.editor.cursorUp"), "the tui table is kept");
    }

    #[test]
    fn windows_dialect_swaps_the_platform_specific_defaults() {
        let definitions = keybindings_for("linux", true);
        assert_eq!(definitions["app.model.cycleBackward"].default_keys, Keys::One("alt+p".to_string()));
        assert_eq!(definitions["app.message.dequeue"].default_keys, Keys::One("alt+q".to_string()));
        assert_eq!(definitions["app.clipboard.pasteImage"].default_keys, Keys::One("alt+v".to_string()));
        assert_eq!(definitions["tui.editor.undo"].default_keys, Keys::One("alt+z".to_string()));
        assert_eq!(definitions["tui.altScreen.search"].default_keys, Keys::One("ctrl+f".to_string()));
        assert_eq!(definitions["app.suspend"].default_keys, Keys::One("ctrl+z".to_string()));
    }

    #[test]
    fn darwin_orders_the_tree_navigation_keys_differently() {
        let definitions = keybindings_for("darwin", false);
        assert_eq!(
            definitions["app.tree.foldOrUp"].default_keys,
            Keys::Many(vec!["alt+left".to_string(), "ctrl+left".to_string()])
        );
        assert_eq!(definitions["tui.editor.undo"].default_keys, Keys::One("ctrl+-".to_string()));
    }

    #[test]
    fn win32_disables_suspend() {
        let definitions = keybindings_for("win32", true);
        assert_eq!(definitions["app.suspend"].default_keys, Keys::Many(Vec::new()));
        assert_eq!(definitions["tui.editor.undo"].default_keys, Keys::One("ctrl+z".to_string()));
    }

    #[test]
    fn windows_keybindings_follow_wsl_markers() {
        let mut env = HashMap::new();
        assert!(!use_windows_keybindings("linux", &env));
        env.insert("WSL_DISTRO_NAME".to_string(), "Ubuntu".to_string());
        assert!(use_windows_keybindings("linux", &env));
        assert!(use_windows_keybindings("win32", &HashMap::new()));
        assert!(!use_windows_keybindings("darwin", &env));
    }

    #[test]
    fn migration_renames_legacy_keys_and_marks_the_result() {
        let migrated = migrate_keybindings_config(&raw(&[("interrupt", json!("ctrl+q"))]));
        assert!(migrated.migrated);
        assert_eq!(migrated.config.get("app.interrupt"), Some(&json!("ctrl+q")));
        assert!(!migrated.config.contains_key("interrupt"));
    }

    #[test]
    fn migration_drops_a_legacy_key_when_its_target_is_already_present() {
        let migrated = migrate_keybindings_config(&raw(&[
            ("interrupt", json!("ctrl+q")),
            ("app.interrupt", json!("escape")),
        ]));
        assert!(migrated.migrated);
        assert_eq!(migrated.config.get("app.interrupt"), Some(&json!("escape")));
        assert!(!migrated.config.contains_key("interrupt"));
    }

    #[test]
    fn migration_leaves_current_keys_alone_and_reports_no_change() {
        let migrated = migrate_keybindings_config(&raw(&[("app.interrupt", json!("ctrl+q"))]));
        assert!(!migrated.migrated);
        assert_eq!(migrated.config.get("app.interrupt"), Some(&json!("ctrl+q")));
    }

    #[test]
    fn ordering_puts_known_ids_in_table_order_then_sorted_extras() {
        let ordered = order_keybindings_config(&raw(&[
            ("zzz.custom", json!("a")),
            ("app.interrupt", json!("b")),
            ("aaa.custom", json!("c")),
        ]));
        let keys: Vec<&String> = ordered.keys().collect();
        assert_eq!(keys.first().map(|key| key.as_str()), Some("app.interrupt"));
        assert_eq!(keys.get(1).map(|key| key.as_str()), Some("aaa.custom"));
        assert_eq!(keys.get(2).map(|key| key.as_str()), Some("zzz.custom"));
    }

    #[test]
    fn a_config_value_must_be_a_string_or_a_string_array() {
        let config = to_keybindings_config(&raw(&[
            ("app.interrupt", json!("escape")),
            ("app.clear", json!(["ctrl+c", "ctrl+x"])),
            ("app.exit", json!(5)),
            ("app.suspend", json!(["ctrl+z", 1])),
        ]));
        assert_eq!(config.get("app.interrupt"), Some(&Some(Keys::One("escape".to_string()))));
        assert_eq!(
            config.get("app.clear"),
            Some(&Some(Keys::Many(vec!["ctrl+c".to_string(), "ctrl+x".to_string()])))
        );
        assert_eq!(config.get("app.exit"), None);
        assert_eq!(config.get("app.suspend"), None);
    }

    #[test]
    fn a_manager_created_without_a_file_has_no_user_bindings() {
        let manager = KeybindingsManager::create(Some("/nonexistent-agent-dir-for-tests"));
        let effective = manager.get_effective_config();
        assert_eq!(effective.get("app.interrupt"), Some(&Some(Keys::One("escape".to_string()))));
        assert_eq!(effective.len(), keybindings().len());
    }

    #[test]
    fn loading_a_malformed_config_file_yields_no_bindings() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("keybindings.json");
        std::fs::write(&path, "{not json").expect("write");
        assert_eq!(KeybindingsManager::load_from_file(&path.to_string_lossy()), KeybindingsConfig::new());
    }

    #[test]
    fn loading_a_file_applies_migration_and_ordering() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("keybindings.json");
        std::fs::write(&path, "{\n  \"interrupt\": \"ctrl+q\",\n  \"app.clear\": \"ctrl+b\"\n}").expect("write");
        let config = KeybindingsManager::load_from_file(&path.to_string_lossy());
        let keys: Vec<&String> = config.keys().collect();
        assert_eq!(keys, vec![&"app.interrupt".to_string(), &"app.clear".to_string()]);
        assert_eq!(config.get("app.interrupt"), Some(&Some(Keys::One("ctrl+q".to_string()))));
    }
}
