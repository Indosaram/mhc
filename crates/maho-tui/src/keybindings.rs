//! Keybinding registry (port of senpi `keybindings.ts`). Ids are strings such as
//! `tui.editor.cursorUp`; downstream crates register their own definitions alongside
//! [`tui_keybindings`].

use std::sync::{Arc, LazyLock, RwLock};

use indexmap::{IndexMap, IndexSet};

use crate::keys::{KeyId, matches_key};

/// Default keys of a keybinding: one key or a list (JS `KeyId | KeyId[]`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Keys {
    One(KeyId),
    Many(Vec<KeyId>),
}

impl Keys {
    fn as_slice(&self) -> &[KeyId] {
        match self {
            Self::One(key) => std::slice::from_ref(key),
            Self::Many(keys) => keys,
        }
    }
}

impl From<&str> for Keys {
    fn from(key: &str) -> Self {
        Self::One(key.to_string())
    }
}

impl<const N: usize> From<[&str; N]> for Keys {
    fn from(keys: [&str; N]) -> Self {
        Self::Many(keys.iter().map(|k| (*k).to_string()).collect())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeybindingDefinition {
    pub default_keys: Keys,
    pub description: Option<String>,
}

/// Definitions in declaration order (JS object key order).
pub type KeybindingDefinitions = IndexMap<String, KeybindingDefinition>;
/// User overrides; `None` mirrors an explicit `undefined` entry (falls back to defaults).
pub type KeybindingsConfig = IndexMap<String, Option<Keys>>;

fn def(default_keys: impl Into<Keys>, description: &str) -> KeybindingDefinition {
    KeybindingDefinition {
        default_keys: default_keys.into(),
        description: Some(description.to_string()),
    }
}

fn none() -> Keys {
    Keys::Many(Vec::new())
}

/// senpi's `TUI_KEYBINDINGS`.
pub fn tui_keybindings() -> KeybindingDefinitions {
    let entries: Vec<(&str, KeybindingDefinition)> = vec![
        ("tui.editor.cursorUp", def("up", "Move cursor up")),
        ("tui.editor.cursorDown", def("down", "Move cursor down")),
        (
            "tui.editor.historyPrevious",
            def(none(), "Select previous prompt history entry"),
        ),
        (
            "tui.editor.historyNext",
            def(none(), "Select next prompt history entry"),
        ),
        (
            "tui.editor.cursorLeft",
            def(["left", "ctrl+b"], "Move cursor left"),
        ),
        (
            "tui.editor.cursorRight",
            def(["right", "ctrl+f"], "Move cursor right"),
        ),
        (
            "tui.editor.cursorWordLeft",
            def(["alt+left", "ctrl+left", "alt+b"], "Move cursor word left"),
        ),
        (
            "tui.editor.cursorWordRight",
            def(
                ["alt+right", "ctrl+right", "alt+f"],
                "Move cursor word right",
            ),
        ),
        (
            "tui.editor.cursorLineStart",
            def(["home", "ctrl+home", "ctrl+a"], "Move to line start"),
        ),
        (
            "tui.editor.cursorLineEnd",
            def(["end", "ctrl+end", "ctrl+e"], "Move to line end"),
        ),
        (
            "tui.editor.jumpForward",
            def("ctrl+]", "Jump forward to character"),
        ),
        (
            "tui.editor.jumpBackward",
            def("ctrl+alt+]", "Jump backward to character"),
        ),
        (
            "tui.editor.pageUp",
            def(["pageUp", "ctrl+pageUp"], "Page up"),
        ),
        (
            "tui.editor.pageDown",
            def(["pageDown", "ctrl+pageDown"], "Page down"),
        ),
        (
            "tui.editor.deleteCharBackward",
            def("backspace", "Delete character backward"),
        ),
        (
            "tui.editor.deleteCharForward",
            def(["delete", "ctrl+d"], "Delete character forward"),
        ),
        (
            "tui.editor.deleteWordBackward",
            def(["ctrl+w", "alt+backspace"], "Delete word backward"),
        ),
        (
            "tui.editor.deleteWordForward",
            def(["alt+d", "alt+delete"], "Delete word forward"),
        ),
        (
            "tui.editor.deleteToLineStart",
            def("ctrl+u", "Delete to line start"),
        ),
        (
            "tui.editor.deleteToLineEnd",
            def("ctrl+k", "Delete to line end"),
        ),
        ("tui.editor.yank", def("ctrl+y", "Yank")),
        ("tui.editor.yankPop", def("alt+y", "Yank pop")),
        ("tui.editor.undo", def("ctrl+-", "Undo")),
        (
            "tui.input.newLine",
            def(["shift+enter", "ctrl+j"], "Insert newline"),
        ),
        ("tui.input.submit", def("enter", "Submit input")),
        ("tui.input.tab", def("tab", "Tab / autocomplete")),
        ("tui.input.copy", def("ctrl+c", "Copy selection")),
        ("tui.select.up", def("up", "Move selection up")),
        ("tui.select.down", def("down", "Move selection down")),
        ("tui.select.pageUp", def("pageUp", "Selection page up")),
        (
            "tui.select.pageDown",
            def("pageDown", "Selection page down"),
        ),
        ("tui.select.confirm", def("enter", "Confirm selection")),
        (
            "tui.select.cancel",
            def(["escape", "ctrl+c"], "Cancel selection"),
        ),
        // These intentionally shadow the unmodified editor bindings in fullscreen mode.
        (
            "tui.altScreen.pageUp",
            def("pageUp", "Scroll viewport up one page"),
        ),
        (
            "tui.altScreen.pageDown",
            def("pageDown", "Scroll viewport down one page"),
        ),
        (
            "tui.altScreen.halfPageUp",
            def(none(), "Scroll viewport up half a page"),
        ),
        (
            "tui.altScreen.halfPageDown",
            def(none(), "Scroll viewport down half a page"),
        ),
        (
            "tui.altScreen.lineUp",
            def(none(), "Scroll viewport up one line"),
        ),
        (
            "tui.altScreen.lineDown",
            def(none(), "Scroll viewport down one line"),
        ),
        (
            "tui.altScreen.previousPrompt",
            def(
                ["ctrl+shift+up", "ctrl+up"],
                "Jump to previous semantic prompt",
            ),
        ),
        (
            "tui.altScreen.nextPrompt",
            def(
                ["ctrl+shift+down", "ctrl+down"],
                "Jump to next semantic prompt",
            ),
        ),
        (
            "tui.altScreen.search",
            def("ctrl+shift+f", "Search the primary scroll view"),
        ),
        (
            "tui.altScreen.searchNext",
            def(["enter", "ctrl+g"], "Select the next search match"),
        ),
        (
            "tui.altScreen.searchPrevious",
            def(
                ["shift+enter", "ctrl+shift+g"],
                "Select the previous search match",
            ),
        ),
        (
            "tui.altScreen.searchClose",
            def("escape", "Close transcript search"),
        ),
        ("tui.altScreen.top", def("home", "Scroll viewport to top")),
        (
            "tui.altScreen.bottom",
            def("end", "Scroll viewport to bottom"),
        ),
    ];
    entries
        .into_iter()
        .map(|(id, d)| (id.to_string(), d))
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeybindingConflict {
    pub key: KeyId,
    pub keybindings: Vec<String>,
}

fn normalize_keys(keys: Option<&Keys>) -> Vec<KeyId> {
    let Some(keys) = keys else {
        return Vec::new();
    };
    let unique: IndexSet<&KeyId> = keys.as_slice().iter().collect();
    unique.into_iter().cloned().collect()
}

#[derive(Debug, Clone)]
pub struct KeybindingsManager {
    definitions: KeybindingDefinitions,
    user_bindings: KeybindingsConfig,
    keys_by_id: IndexMap<String, Vec<KeyId>>,
    conflicts: Vec<KeybindingConflict>,
}

impl KeybindingsManager {
    pub fn new(definitions: KeybindingDefinitions, user_bindings: KeybindingsConfig) -> Self {
        let mut manager = Self {
            definitions,
            user_bindings,
            keys_by_id: IndexMap::new(),
            conflicts: Vec::new(),
        };
        manager.rebuild();
        manager
    }

    fn rebuild(&mut self) {
        self.keys_by_id.clear();
        self.conflicts.clear();

        let mut user_claims: IndexMap<KeyId, IndexSet<String>> = IndexMap::new();
        for (keybinding, keys) in &self.user_bindings {
            if !self.definitions.contains_key(keybinding) {
                continue;
            }
            for key in normalize_keys(keys.as_ref()) {
                user_claims
                    .entry(key)
                    .or_default()
                    .insert(keybinding.clone());
            }
        }
        for (key, keybindings) in user_claims {
            if keybindings.len() > 1 {
                self.conflicts.push(KeybindingConflict {
                    key,
                    keybindings: keybindings.into_iter().collect(),
                });
            }
        }

        for (id, definition) in &self.definitions {
            let keys = match self.user_bindings.get(id) {
                Some(Some(user_keys)) => normalize_keys(Some(user_keys)),
                _ => normalize_keys(Some(&definition.default_keys)),
            };
            self.keys_by_id.insert(id.clone(), keys);
        }
    }

    pub fn matches(&self, data: &str, keybinding: &str) -> bool {
        self.keys_by_id
            .get(keybinding)
            .is_some_and(|keys| keys.iter().any(|key| matches_key(data, key)))
    }

    pub fn get_keys(&self, keybinding: &str) -> Vec<KeyId> {
        self.keys_by_id.get(keybinding).cloned().unwrap_or_default()
    }

    pub fn get_definition(&self, keybinding: &str) -> Option<&KeybindingDefinition> {
        self.definitions.get(keybinding)
    }

    pub fn get_conflicts(&self) -> Vec<KeybindingConflict> {
        self.conflicts.clone()
    }

    pub fn set_user_bindings(&mut self, user_bindings: KeybindingsConfig) {
        self.user_bindings = user_bindings;
        self.rebuild();
    }

    pub fn get_user_bindings(&self) -> KeybindingsConfig {
        self.user_bindings.clone()
    }

    pub fn get_resolved_bindings(&self) -> KeybindingsConfig {
        self.definitions
            .keys()
            .map(|id| {
                let keys = self.keys_by_id.get(id).cloned().unwrap_or_default();
                let resolved = match <[KeyId; 1]>::try_from(keys) {
                    Ok([one]) => Keys::One(one),
                    Err(keys) => Keys::Many(keys),
                };
                (id.clone(), Some(resolved))
            })
            .collect()
    }
}

static GLOBAL_KEYBINDINGS: LazyLock<RwLock<Option<Arc<KeybindingsManager>>>> =
    LazyLock::new(|| RwLock::new(None));

pub fn set_keybindings(keybindings: KeybindingsManager) {
    let mut global = GLOBAL_KEYBINDINGS
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *global = Some(Arc::new(keybindings));
}

/// The process-wide manager, created lazily from [`tui_keybindings`].
pub fn get_keybindings() -> Arc<KeybindingsManager> {
    if let Some(existing) = GLOBAL_KEYBINDINGS
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
    {
        return Arc::clone(existing);
    }
    let mut global = GLOBAL_KEYBINDINGS
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    Arc::clone(global.get_or_insert_with(|| {
        Arc::new(KeybindingsManager::new(tui_keybindings(), IndexMap::new()))
    }))
}

#[cfg(test)]
#[path = "keybindings_tests.rs"]
mod tests;
