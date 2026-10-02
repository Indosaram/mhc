use maho_interactive::{components::{shortcut_overlay::*, show_images_selector::ShowImagesSelectorComponent, thinking_selector::{ThinkingSelectorComponent, ThinkingSelectorOptions}, trust_selector::{TrustSelectorComponent, TrustSelectorOptions}, custom_editor::{CustomEditor, CustomEditorOptions}, extension_editor::editor_theme}, theme::{Theme, ColorMode}};
use maho_tui::tui::Component;

struct EditorHost;
impl maho_tui::components::editor::EditorTuiHost for EditorHost {
    fn request_render(&self) {}
    fn terminal_rows(&self) -> usize { 36 }
}

fn keybindings() -> std::sync::Arc<maho_tui::keybindings::KeybindingsManager> {
    std::sync::Arc::new(maho_tui::keybindings::KeybindingsManager::new(maho_core::keybindings::keybindings().clone(), Default::default()))
}

#[test]
fn helper_renders_equal_pinned_senpi_at_three_widths() {
    let cases: serde_json::Value = serde_json::from_str(include_str!("golden/task35-helpers.json")).expect("golden");
    let theme = Theme::builtin("dark", ColorMode::Truecolor).expect("theme");
    for case in cases.as_array().expect("cases") {
        let width = usize::try_from(case["width"].as_u64().expect("width")).expect("width fits");
        let lines = match case["kind"].as_str().expect("kind") {
            "shortcut" => ShortcutOverlay::new(&theme).render(width),
            "images" => ShowImagesSelectorComponent::new(&theme, case["current"].as_bool().expect("current"), Box::new(|_| {}), Box::new(|| {})).render(width),
            "thinking" => ThinkingSelectorComponent::new(&theme, keybindings(), ThinkingSelectorOptions {
                current: maho_ai::types::ModelThinkingLevel::Medium, available: maho_ai::types::ModelThinkingLevel::ALL.to_vec(), default: Some(maho_ai::types::ModelThinkingLevel::High), on_select: Box::new(|_| {}), on_cancel: Box::new(|| {}), on_select_as_default: Some(Box::new(|_| {})),
            }).render(width),
            "trust" => TrustSelectorComponent::new(&theme, keybindings(), TrustSelectorOptions { cwd: "/tmp".into(), saved_decision: None, project_trusted: false, on_select: Box::new(|_| {}), on_cancel: Box::new(|| {}) }).render(width),
            "editor" => {
                let mut editor = CustomEditor::new(std::rc::Rc::new(EditorHost), editor_theme(&theme), keybindings(), CustomEditorOptions::default());
                editor.editor.set_text("hello 한국어");
                editor.render(width)
            }
            "scoped" => {
                use maho_interactive::components::scoped_models_selector::*;
                let models = ["a", "b"].map(|id| serde_json::from_value(serde_json::json!({ "id": id, "name": format!("Model {id}"), "provider": "test", "api": "anthropic-messages", "baseUrl": "https://example.test", "reasoning": false, "input": ["text"], "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 }, "contextWindow": 200000, "maxTokens": 8192 })).expect("model"));
                ScopedModelsSelectorComponent::new(&theme, keybindings(), ModelsConfig { all_models: models.to_vec(), enabled_model_ids: Some(vec!["test/b".into(), "missing/model".into()]), refresh_status: None }, ModelsCallbacks { on_change: Box::new(|_| {}), on_persist: Box::new(|_| {}), on_cancel: Box::new(|| {}) }).render(width)
            }
            kind => panic!("unknown golden kind {kind}"),
        };
        assert_eq!(serde_json::to_value(lines).expect("serialize"), case["expected"], "{case}");
    }
}

#[test]
fn shortcut_overlay_opens_only_for_typed_question_on_empty_editor() {
    assert!(should_show_shortcut_overlay("", "?", EditorInputKind::Typed));
    assert!(!should_show_shortcut_overlay("", "?", EditorInputKind::Paste));
    assert!(!should_show_shortcut_overlay("a", "?", EditorInputKind::Typed));
    assert!(!should_show_shortcut_overlay("", "?", EditorInputKind::Other));
}

#[test]
fn paste_classification_matches_utf16_delta_and_out_of_band_signal() {
    assert_eq!(classify_editor_input("", "?", false), EditorInputKind::Typed);
    assert_eq!(classify_editor_input("", "ab", false), EditorInputKind::Paste);
    assert_eq!(classify_editor_input("", "?", true), EditorInputKind::Paste);
    assert_eq!(classify_editor_input("", "😀", false), EditorInputKind::Paste);
}

#[test]
fn images_selector_calls_current_choice_on_enter() {
    use std::{cell::RefCell, rc::Rc};
    let selected = Rc::new(RefCell::new(None));
    let captured = selected.clone();
    let theme = Theme::builtin("dark", ColorMode::Truecolor).expect("theme");
    let mut selector = ShowImagesSelectorComponent::new(&theme, false, Box::new(move |value| *captured.borrow_mut() = Some(value)), Box::new(|| {}));
    selector.handle_input("\r");
    assert_eq!(*selected.borrow(), Some(false));
}

#[test]
fn hidden_diagnostics_are_redacted_and_private() {
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("nested/debug.log");
    maho_interactive::interactive_stderr_guard::append_hidden_interactive_stderr(&path, "SECRET_TOKEN=test-value", "2026-01-01T00:00:00.000Z").expect("write");
    let data = std::fs::read_to_string(&path).expect("read");
    assert!(!data.contains("test-value"));
    assert!(data.contains("SECRET_TOKEN=[REDACTED]"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(path).expect("metadata").permissions().mode() & 0o777, 0o600);
    }
}
