//! Port of senpi packages/tui/test/keybindings.test.ts.

use indexmap::IndexMap;

use super::*;

fn manager(user: &[(&str, Keys)]) -> KeybindingsManager {
    let user_bindings: KeybindingsConfig = user
        .iter()
        .map(|(id, keys)| ((*id).to_string(), Some(keys.clone())))
        .collect();
    KeybindingsManager::new(tui_keybindings(), user_bindings)
}

fn keys(manager: &KeybindingsManager, id: &str) -> Vec<String> {
    manager.get_keys(id)
}

#[test]
fn binds_ctrl_j_as_a_default_newline_alias() {
    let keybindings = manager(&[]);
    assert_eq!(
        keys(&keybindings, "tui.input.newLine"),
        ["shift+enter", "ctrl+j"]
    );
    assert!(keybindings.matches("\n", "tui.input.newLine"));
    assert!(keybindings.matches("\x1b[106;5u", "tui.input.newLine"));
}

#[test]
fn binds_modified_and_unmodified_editor_viewport_navigation() {
    let keybindings = manager(&[]);
    assert_eq!(
        keys(&keybindings, "tui.editor.cursorLineStart"),
        ["home", "ctrl+home", "ctrl+a"]
    );
    assert_eq!(
        keys(&keybindings, "tui.editor.cursorLineEnd"),
        ["end", "ctrl+end", "ctrl+e"]
    );
    assert_eq!(
        keys(&keybindings, "tui.editor.pageUp"),
        ["pageUp", "ctrl+pageUp"]
    );
    assert_eq!(
        keys(&keybindings, "tui.editor.pageDown"),
        ["pageDown", "ctrl+pageDown"]
    );
}

#[test]
fn leaves_dedicated_prompt_history_navigation_unbound_by_default() {
    let keybindings = manager(&[]);
    assert!(keys(&keybindings, "tui.editor.historyPrevious").is_empty());
    assert!(keys(&keybindings, "tui.editor.historyNext").is_empty());
}

#[test]
fn binds_unmodified_terminal_viewport_shortcuts_to_alternate_screen_navigation() {
    let keybindings = manager(&[]);
    let empty: [&str; 0] = [];
    assert_eq!(keys(&keybindings, "tui.altScreen.pageUp"), ["pageUp"]);
    assert_eq!(keys(&keybindings, "tui.altScreen.pageDown"), ["pageDown"]);
    assert_eq!(keys(&keybindings, "tui.altScreen.halfPageUp"), empty);
    assert_eq!(keys(&keybindings, "tui.altScreen.halfPageDown"), empty);
    assert_eq!(keys(&keybindings, "tui.altScreen.lineUp"), empty);
    assert_eq!(keys(&keybindings, "tui.altScreen.lineDown"), empty);
    assert_eq!(
        keys(&keybindings, "tui.altScreen.previousPrompt"),
        ["ctrl+shift+up", "ctrl+up"]
    );
    assert_eq!(
        keys(&keybindings, "tui.altScreen.nextPrompt"),
        ["ctrl+shift+down", "ctrl+down"]
    );
    assert_eq!(keys(&keybindings, "tui.altScreen.search"), ["ctrl+shift+f"]);
    assert_eq!(
        keys(&keybindings, "tui.altScreen.searchNext"),
        ["enter", "ctrl+g"]
    );
    assert_eq!(
        keys(&keybindings, "tui.altScreen.searchPrevious"),
        ["shift+enter", "ctrl+shift+g"]
    );
    assert_eq!(keys(&keybindings, "tui.altScreen.searchClose"), ["escape"]);
    assert_eq!(keys(&keybindings, "tui.altScreen.top"), ["home"]);
    assert_eq!(keys(&keybindings, "tui.altScreen.bottom"), ["end"]);
}

#[test]
fn does_not_evict_selector_confirm_when_input_submit_is_rebound() {
    let keybindings = manager(&[("tui.input.submit", Keys::from(["enter", "ctrl+enter"]))]);
    assert_eq!(
        keys(&keybindings, "tui.input.submit"),
        ["enter", "ctrl+enter"]
    );
    assert_eq!(keys(&keybindings, "tui.select.confirm"), ["enter"]);
}

#[test]
fn does_not_evict_cursor_bindings_when_another_action_reuses_the_same_key() {
    let keybindings = manager(&[("tui.select.up", Keys::from(["up", "ctrl+p"]))]);
    assert_eq!(keys(&keybindings, "tui.select.up"), ["up", "ctrl+p"]);
    assert_eq!(keys(&keybindings, "tui.editor.cursorUp"), ["up"]);
}

#[test]
fn still_reports_direct_user_binding_conflicts_without_evicting_defaults() {
    let keybindings = manager(&[
        ("tui.input.submit", Keys::from("ctrl+x")),
        ("tui.select.confirm", Keys::from("ctrl+x")),
    ]);
    assert_eq!(
        keybindings.get_conflicts(),
        vec![KeybindingConflict {
            key: "ctrl+x".to_string(),
            keybindings: vec![
                "tui.input.submit".to_string(),
                "tui.select.confirm".to_string()
            ],
        }]
    );
    assert_eq!(
        keys(&keybindings, "tui.editor.cursorLeft"),
        ["left", "ctrl+b"]
    );
    let _: IndexMap<String, Option<Keys>> = keybindings.get_user_bindings();
}
