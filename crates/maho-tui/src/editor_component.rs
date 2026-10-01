//! Port of senpi `packages/tui/src/editor-component.ts`.
//!
//! Interface for custom editor components: extensions may provide their own editor implementation
//! (vim mode, emacs mode, custom keybindings) while keeping the core application's contract.
//! senpi's optional members are modelled as trait methods with default `None` returns.

use crate::autocomplete::{AutocompleteItem, AutocompleteProvider, ApplyCompletionResult, MentionRange};
use crate::image_markers::EditorImageState;
use crate::paste_markers::EditorPasteState;
use crate::tui::Component;

/// A custom editor component; superset of [`Component`].
pub trait EditorComponent: Component {
    fn get_text(&self) -> String;

    fn set_text(&mut self, text: &str);

    /// Add text to history for up/down navigation.
    fn add_to_history(&mut self, _text: &str) {}

    /// Insert text at the current cursor position.
    fn insert_text_at_cursor(&mut self, _text: &str) {}

    /// Text with any markers expanded (for example paste markers).
    fn get_expanded_text(&self) -> Option<String> {
        None
    }

    /// Snapshot the paste-marker registry so collapsed markers can be transferred to another
    /// editor instance alongside `get_text`.
    fn get_paste_state(&self) -> Option<EditorPasteState> {
        None
    }

    /// Install a paste-marker registry snapshot taken from another editor instance.
    fn set_paste_state(&mut self, _state: &EditorPasteState) {}

    /// Insert the next atomic `[Image #N]` marker at the cursor and return its id.
    fn insert_image_marker(&mut self) -> Option<u64> {
        None
    }

    /// Snapshot the image-marker registry (ids only, never image bytes).
    fn get_image_marker_state(&self) -> Option<EditorImageState> {
        None
    }

    /// Install an image-marker registry snapshot taken from another editor instance.
    fn set_image_marker_state(&mut self, _state: &EditorImageState) {}

    /// Called with the image-marker ids in text reading order whenever markers change.
    fn on_image_markers_changed(&mut self, _order: &[u64]) {}

    /// Set the autocomplete provider.
    fn set_autocomplete_provider(&mut self, _provider: Box<dyn AutocompleteProvider>) {}

    /// Set horizontal padding.
    fn set_padding_x(&mut self, _padding: usize) {}

    /// Set max visible items in the autocomplete dropdown.
    fn set_autocomplete_max_visible(&mut self, _max_visible: usize) {}
}

/// senpi's `EditorComponent["applyCompletion"]` shape, exported for custom editor implementations.
pub type EditorApplyCompletion = ApplyCompletionResult;
/// senpi's `EditorComponent["getMentionRanges"]` shape.
pub type EditorMentionRange = MentionRange;
/// senpi's `EditorComponent["insertImageMarker"]` shape.
pub type EditorImageMarkerId = u64;
/// senpi's `EditorComponent["getText"]` completion item shape.
pub type EditorAutocompleteItem = AutocompleteItem;
