use maho_tui::{editor_component::EditorComponent, paste_markers::expand_paste_markers};

pub fn strip_image_markers(text: &str) -> String {
    let mut remainder = text;
    let mut stripped = String::new();
    while let Some(index) = remainder.find("[Image #") {
        stripped.push_str(&remainder[..index]);
        remainder = &remainder[index..];
        let suffix = &remainder[8..];
        let digits = suffix.bytes().take_while(u8::is_ascii_digit).count();
        if digits > 0 && !suffix.starts_with('0') && suffix.as_bytes().get(digits) == Some(&b']') {
            remainder = &suffix[digits + 1..];
        } else {
            stripped.push('[');
            remainder = &remainder[1..];
        }
    }
    stripped.push_str(remainder);
    let mut collapsed = String::new();
    let mut whitespace = String::new();
    for character in stripped.chars() {
        if matches!(character, ' ' | '\t') {
            whitespace.push(character);
        } else {
            if whitespace.len() > 1 {
                collapsed.push(' ');
            } else {
                collapsed.push_str(&whitespace);
            }
            whitespace.clear();
            collapsed.push(character);
        }
    }
    collapsed.push_str(&whitespace);
    collapsed.trim().to_owned()
}

/// Which of senpi's optional `EditorComponent` methods an editor provides. senpi checks each
/// getter and setter separately (`target.getPasteState && target.setPasteState`), so an editor can
/// expose a registry it cannot install, or the other way around.
pub struct EditorCapabilities {
    pub paste_markers: bool,
    pub set_paste_markers: bool,
    pub image_markers: bool,
    pub own_image_markers: bool,
    pub expanded_text: bool,
}

pub fn editor_capabilities(editor: &dyn EditorComponent) -> EditorCapabilities {
    EditorCapabilities {
        paste_markers: editor.get_paste_state().is_some(),
        set_paste_markers: editor.can_set_paste_state(),
        image_markers: editor.get_image_marker_state().is_some(),
        own_image_markers: editor.can_own_image_markers(),
        expanded_text: editor.get_expanded_text().is_some(),
    }
}

/// senpi `transferEditorContent`: move the draft (and any collapsed markers) from one editor to
/// another. Returns whether the image markers survived the move: image payloads live outside the
/// editor, so the caller must drop them when the destination cannot own the markers.
pub fn transfer_editor_content(
    source: &dyn EditorComponent,
    target: &mut dyn EditorComponent,
) -> bool {
    let raw = source.get_text();
    let pastes = source.get_paste_state();
    let images = source.get_image_marker_state();
    let images_transferred = images.is_some()
        && target.get_image_marker_state().is_some()
        && target.can_own_image_markers();
    let keep = |text: String| {
        if images_transferred {
            text
        } else {
            strip_image_markers(&text)
        }
    };
    match pastes {
        Some(state) if target.get_paste_state().is_some() && target.can_set_paste_state() => {
            target.set_text(&keep(raw));
            target.set_paste_state(&state);
        }
        pastes => {
            let expanded = source.get_expanded_text().unwrap_or_else(|| {
                pastes
                    .as_ref()
                    .map_or_else(|| raw.clone(), |state| expand_paste_markers(&raw, state))
            });
            target.set_text(&keep(expanded));
        }
    }
    if images_transferred && let Some(state) = images {
        target.set_image_marker_state(&state);
    }
    images_transferred
}

pub fn expand_editor_submission(editor: &dyn EditorComponent, text: &str) -> String {
    editor.get_expanded_text().unwrap_or_else(|| {
        editor.get_paste_state().map_or_else(
            || text.to_owned(),
            |state| expand_paste_markers(text, &state),
        )
    })
}

pub fn expand_submitted_text(editor: &dyn EditorComponent, text: &str) -> String {
    editor
        .get_expanded_text()
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| {
            editor.get_paste_state().map_or_else(
                || text.to_owned(),
                |state| expand_paste_markers(text, &state),
            )
        })
}

#[cfg(test)]
mod capability_tests {
    use super::*;
    use maho_tui::editor_component::EditorComponent;

    struct CapabilityEditor;
    impl maho_tui::tui::Component for CapabilityEditor {
        fn render(&mut self, _width: usize) -> Vec<String> {
            Vec::new()
        }
    }
    impl EditorComponent for CapabilityEditor {
        fn get_text(&self) -> String {
            String::new()
        }
        fn set_text(&mut self, _text: &str) {}
    }

    #[test]
    fn capability_flags_are_independent() {
        let capabilities = editor_capabilities(&CapabilityEditor);
        assert!(!capabilities.paste_markers);
        assert!(!capabilities.set_paste_markers);
        assert!(!capabilities.image_markers);
        assert!(!capabilities.own_image_markers);
        assert!(!capabilities.expanded_text);
    }

    /// An editor that reports paste and image registries but cannot install either.
    struct ReadOnlyRegistryEditor {
        text: String,
    }
    impl maho_tui::tui::Component for ReadOnlyRegistryEditor {
        fn render(&mut self, _width: usize) -> Vec<String> {
            Vec::new()
        }
    }
    impl EditorComponent for ReadOnlyRegistryEditor {
        fn get_text(&self) -> String {
            self.text.clone()
        }
        fn set_text(&mut self, text: &str) {
            self.text = text.to_owned();
        }
        fn get_paste_state(&self) -> Option<maho_tui::paste_markers::EditorPasteState> {
            Some(Default::default())
        }
        fn can_set_paste_state(&self) -> bool {
            false
        }
        fn get_image_marker_state(&self) -> Option<maho_tui::image_markers::EditorImageState> {
            Some(Default::default())
        }
        fn can_own_image_markers(&self) -> bool {
            false
        }
    }

    #[test]
    fn getter_without_setter_does_not_count_as_a_capability() {
        let editor = ReadOnlyRegistryEditor {
            text: String::new(),
        };
        let capabilities = editor_capabilities(&editor);
        assert!(capabilities.paste_markers && !capabilities.set_paste_markers);
        assert!(capabilities.image_markers && !capabilities.own_image_markers);
    }

    #[test]
    fn transfer_strips_image_markers_a_target_cannot_own() {
        let source = ReadOnlyRegistryEditor {
            text: "draft [Image #1] text".to_owned(),
        };
        let mut target = ReadOnlyRegistryEditor {
            text: String::new(),
        };
        assert!(!transfer_editor_content(&source, &mut target));
        assert_eq!(target.text, "draft text");
    }
}
