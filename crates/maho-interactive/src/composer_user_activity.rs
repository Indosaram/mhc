//! Port of senpi `modes/interactive/composer-user-activity.ts` (pin 903b8336).
//!
//! senpi swaps `editor.handleInput` for a wrapper that reports real user edits (#3140). Rust
//! cannot reassign a method, so [`ObservedComposer`] wraps the editor instead and forwards every
//! [`Component`] and [`EditorComponent`] call to it:
//!
//! - [`ObservedComposer::handle_input`] is `observeComposerActivity`'s wrapper. Input that changes
//!   the text reports activity. Restoring a draft through `set_text`, moving the cursor, or
//!   submitting (which clears the editor) does not.
//! - [`ObservedComposer::insert_programmatic_input`] is `insertProgrammaticComposerInput`:
//!   extension-supplied text updates the composer but is not terminal input.
//!
//! senpi briefly wraps `editor.onSubmit` to see a submission. Here the submit callback is
//! installed through the wrapper's [`EditorComponent::set_on_submit`], which records the
//! submission itself.

use std::cell::Cell;
use std::rc::Rc;

use maho_tui::autocomplete::AutocompleteProvider;
use maho_tui::components::editor::{
    ChangeCallback, ImageMarkersChangedCallback, RestoreAttachmentCallback,
    SnapshotAttachmentCallback, StyleFn, SubmitCallback,
};
use maho_tui::editor_component::EditorComponent;
use maho_tui::image_markers::EditorImageState;
use maho_tui::paste_markers::EditorPasteState;
use maho_tui::tui::{Component, TuiMouseEvent, TuiMouseEventResult};

pub type ActivityCallback = Box<dyn FnMut()>;

/// senpi `observeComposerActivity` applied to `editor`.
pub struct ObservedComposer<E: EditorComponent> {
    editor: E,
    on_activity: ActivityCallback,
    submitted: Rc<Cell<bool>>,
}

impl<E: EditorComponent> ObservedComposer<E> {
    /// senpi `observeComposerActivity(editor, onActivity)`.
    pub fn new(mut editor: E, on_activity: ActivityCallback) -> Self {
        let submitted = Rc::new(Cell::new(false));
        editor.set_on_submit(Some(submit_marker(Rc::clone(&submitted), None)));
        Self {
            editor,
            on_activity,
            submitted,
        }
    }

    /// senpi `insertProgrammaticComposerInput`: feed `data` to the editor without reporting it as
    /// user activity.
    pub fn insert_programmatic_input(&mut self, data: &str) {
        self.editor.handle_input(data);
    }

    pub fn inner(&self) -> &E {
        &self.editor
    }

    pub fn inner_mut(&mut self) -> &mut E {
        &mut self.editor
    }
}

fn submit_marker(
    submitted: Rc<Cell<bool>>,
    mut callback: Option<SubmitCallback>,
) -> SubmitCallback {
    Box::new(move |text: &str| {
        submitted.set(true);
        if let Some(callback) = callback.as_mut() {
            callback(text);
        }
    })
}

impl<E: EditorComponent> Component for ObservedComposer<E> {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.editor.render(width)
    }
    fn handle_input(&mut self, data: &str) {
        let previous_text = self.editor.get_text();
        self.submitted.set(false);
        self.editor.handle_input(data);
        // Submission clears the editor and is handled by prompt admission instead.
        let submitted = self.submitted.replace(false);
        if !submitted && self.editor.get_text() != previous_text {
            (self.on_activity)();
        }
    }
    fn has_input_handler(&self) -> bool {
        self.editor.has_input_handler()
    }
    fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.editor.handle_mouse(event)
    }
    fn wants_key_release(&self) -> bool {
        self.editor.wants_key_release()
    }
    fn invalidate(&mut self) {
        self.editor.invalidate();
    }
    fn dispose(&mut self) {
        self.editor.dispose();
    }
    fn focusable_get(&self) -> Option<bool> {
        self.editor.focusable_get()
    }
    fn focusable_set(&mut self, focused: bool) {
        self.editor.focusable_set(focused);
    }
    fn component_name(&self) -> &'static str {
        self.editor.component_name()
    }
}

impl<E: EditorComponent> EditorComponent for ObservedComposer<E> {
    fn get_text(&self) -> String {
        self.editor.get_text()
    }
    fn set_text(&mut self, text: &str) {
        self.editor.set_text(text);
    }
    fn set_on_submit(&mut self, callback: Option<SubmitCallback>) {
        self.editor
            .set_on_submit(Some(submit_marker(Rc::clone(&self.submitted), callback)));
    }
    fn set_on_change(&mut self, callback: Option<ChangeCallback>) {
        self.editor.set_on_change(callback);
    }
    fn add_to_history(&mut self, text: &str) {
        self.editor.add_to_history(text);
    }
    fn insert_text_at_cursor(&mut self, text: &str) {
        self.editor.insert_text_at_cursor(text);
    }
    fn get_expanded_text(&self) -> Option<String> {
        self.editor.get_expanded_text()
    }
    fn get_paste_state(&self) -> Option<EditorPasteState> {
        self.editor.get_paste_state()
    }
    fn set_paste_state(&mut self, state: &EditorPasteState) {
        self.editor.set_paste_state(state);
    }
    fn can_set_paste_state(&self) -> bool {
        self.editor.can_set_paste_state()
    }
    fn insert_image_marker(&mut self) -> Option<u64> {
        self.editor.insert_image_marker()
    }
    fn get_image_marker_state(&self) -> Option<EditorImageState> {
        self.editor.get_image_marker_state()
    }
    fn set_image_marker_state(&mut self, state: &EditorImageState) {
        self.editor.set_image_marker_state(state);
    }
    fn can_own_image_markers(&self) -> bool {
        self.editor.can_own_image_markers()
    }
    fn on_image_markers_changed(&mut self, order: &[u64]) {
        self.editor.on_image_markers_changed(order);
    }
    fn set_on_image_markers_changed(&mut self, callback: Option<ImageMarkersChangedCallback>) {
        self.editor.set_on_image_markers_changed(callback);
    }
    fn set_snapshot_attachment_state(&mut self, callback: Option<SnapshotAttachmentCallback>) {
        self.editor.set_snapshot_attachment_state(callback);
    }
    fn set_restore_attachment_state(&mut self, callback: Option<RestoreAttachmentCallback>) {
        self.editor.set_restore_attachment_state(callback);
    }
    fn set_autocomplete_provider(&mut self, provider: Box<dyn AutocompleteProvider>) {
        self.editor.set_autocomplete_provider(provider);
    }
    fn border_color(&self) -> Option<StyleFn> {
        self.editor.border_color()
    }
    fn set_border_color(&mut self, color: StyleFn) {
        self.editor.set_border_color(color);
    }
    fn poll_autocomplete(&mut self) -> bool {
        self.editor.poll_autocomplete()
    }
    fn set_padding_x(&mut self, padding: usize) {
        self.editor.set_padding_x(padding);
    }
    fn set_autocomplete_max_visible(&mut self, max_visible: usize) {
        self.editor.set_autocomplete_max_visible(max_visible);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// A minimal line editor: printable input appends, backspace deletes, the left arrow moves
    /// nothing, and Enter submits and clears, like pi-tui's `Editor.submitValue()`.
    #[derive(Default)]
    struct LineEditor {
        text: String,
        on_submit: Option<SubmitCallback>,
    }

    impl Component for LineEditor {
        fn render(&mut self, _width: usize) -> Vec<String> {
            vec![self.text.clone()]
        }
        fn handle_input(&mut self, data: &str) {
            match data {
                "\r" => {
                    let text = std::mem::take(&mut self.text);
                    if let Some(callback) = self.on_submit.as_mut() {
                        callback(&text);
                    }
                }
                "\x7f" => {
                    self.text.pop();
                }
                "\x1b[D" => {}
                data => self.text.push_str(data),
            }
        }
        fn has_input_handler(&self) -> bool {
            true
        }
    }

    impl EditorComponent for LineEditor {
        fn get_text(&self) -> String {
            self.text.clone()
        }
        fn set_text(&mut self, text: &str) {
            self.text = text.to_owned();
        }
        fn set_on_submit(&mut self, callback: Option<SubmitCallback>) {
            self.on_submit = callback;
        }
    }

    fn observed() -> (ObservedComposer<LineEditor>, Rc<Cell<usize>>) {
        let count = Rc::new(Cell::new(0));
        let seen = Rc::clone(&count);
        let composer = ObservedComposer::new(
            LineEditor::default(),
            Box::new(move || seen.set(seen.get() + 1)),
        );
        (composer, count)
    }

    #[test]
    fn reports_each_keystroke_that_changes_the_text() {
        let (mut composer, activity) = observed();
        composer.handle_input("h");
        composer.handle_input("i");
        assert_eq!(composer.get_text(), "hi");
        assert_eq!(activity.get(), 2);
    }

    #[test]
    fn draft_restoration_and_cursor_movement_are_not_activity() {
        let (mut composer, activity) = observed();
        composer.set_text("restored draft");
        composer.handle_input("\x1b[D");
        assert_eq!(activity.get(), 0);
    }

    #[test]
    fn deleting_a_restored_draft_is_activity() {
        let (mut composer, activity) = observed();
        composer.set_text("restored draft");
        composer.handle_input("\x7f");
        assert_eq!(activity.get(), 1);
    }

    #[test]
    fn submission_is_not_activity_and_still_reaches_the_submit_callback() {
        let (mut composer, activity) = observed();
        let submitted = Rc::new(RefCell::new(Vec::new()));
        let sink = Rc::clone(&submitted);
        composer.set_on_submit(Some(Box::new(move |text: &str| {
            sink.borrow_mut().push(text.to_owned())
        })));
        composer.set_text("ship it");
        composer.handle_input("\r");
        assert_eq!(*submitted.borrow(), ["ship it"]);
        assert_eq!(activity.get(), 0);
        composer.handle_input("x");
        assert_eq!(activity.get(), 1);
    }

    #[test]
    fn programmatic_input_updates_the_composer_without_activity() {
        let (mut composer, activity) = observed();
        composer.insert_programmatic_input("from an extension");
        assert_eq!(composer.get_text(), "from an extension");
        assert_eq!(activity.get(), 0);
    }
}
