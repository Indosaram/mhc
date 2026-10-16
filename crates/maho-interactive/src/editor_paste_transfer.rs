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
            if whitespace.len() > 1 { collapsed.push(' '); } else { collapsed.push_str(&whitespace); }
            whitespace.clear();
            collapsed.push(character);
        }
    }
    collapsed.push_str(&whitespace);
    collapsed.trim().to_owned()
}

pub struct EditorCapabilities { pub paste_markers: bool, pub image_markers: bool, pub expanded_text: bool }

#[cfg(test)]
mod capability_tests {
    use super::*;
    use maho_tui::editor_component::EditorComponent;

    struct CapabilityEditor;
    impl EditorComponent for CapabilityEditor {
        fn get_text(&self) -> String { String::new() }
        fn set_text(&mut self, _text: &str) {}
        fn get_paste_state(&self) -> Option<maho_tui::paste_markers::PasteState> { None }
        fn set_paste_state(&mut self, _state: &maho_tui::paste_markers::PasteState) {}
        fn get_image_marker_state(&self) -> Option<maho_tui::paste_markers::ImageMarkerState> { None }
        fn set_image_marker_state(&mut self, _state: &maho_tui::paste_markers::ImageMarkerState) {}
        fn get_expanded_text(&self) -> Option<String> { None }
    }

    #[test]
    fn capability_flags_are_independent() {
        let capabilities = editor_capabilities(&CapabilityEditor);
        assert!(!capabilities.paste_markers);
        assert!(!capabilities.image_markers);
        assert!(!capabilities.expanded_text);
    }
}

pub fn editor_capabilities(editor: &dyn EditorComponent) -> EditorCapabilities {
    EditorCapabilities { paste_markers: editor.get_paste_state().is_some(), image_markers: editor.get_image_marker_state().is_some(), expanded_text: editor.get_expanded_text().is_some() }
}

pub fn transfer_editor_content(source: &dyn EditorComponent, target: &mut dyn EditorComponent) -> bool {
    let raw = source.get_text();
    let pastes = source.get_paste_state();
    let images = source.get_image_marker_state();
    let images_transferred = images.is_some() && target.get_image_marker_state().is_some();
    let transfer_pastes = pastes.is_some() && target.get_paste_state().is_some();
    let text = if transfer_pastes {
        raw
    } else {
        source.get_expanded_text().unwrap_or_else(|| pastes.as_ref().map_or_else(|| raw.clone(), |state| expand_paste_markers(&raw, state)))
    };
    target.set_text(&if images_transferred { text } else { strip_image_markers(&text) });
    if transfer_pastes && let Some(state) = pastes { target.set_paste_state(&state); }
    if images_transferred && let Some(state) = images { target.set_image_marker_state(&state); }
    images_transferred
}

pub fn expand_editor_submission(editor: &dyn EditorComponent, text: &str) -> String {
    editor.get_expanded_text().unwrap_or_else(|| editor.get_paste_state().map_or_else(|| text.to_owned(), |state| expand_paste_markers(text, &state)))
}

pub fn expand_submitted_text(editor: &dyn EditorComponent, text: &str) -> String {
    editor.get_expanded_text().filter(|text| !text.is_empty()).unwrap_or_else(|| editor.get_paste_state().map_or_else(|| text.to_owned(), |state| expand_paste_markers(text, &state)))
}
