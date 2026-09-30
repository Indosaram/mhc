//! Port of senpi `packages/tui/src/components/editor.ts`.
//!
//! Multiline editor with paste/image markers, history, Emacs kill/yank, an undo stack and
//! `$`/`/`/`@` autocomplete. Behavior of record: `packages/tui/test/editor*.test.ts`,
//! `autocomplete*.test.ts`, `image-markers.test.ts` and `dollar-skill-mentions.test.ts`.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;
use std::sync::LazyLock;

use regex::Regex;

use crate::autocomplete::{AutocompleteItem, AutocompleteProvider, MentionRange};
use crate::components::editor_line_render::{render_editor_line, EditorLineCursor, EditorLineRenderInput};
use crate::components::select_list::{
    SelectItem, SelectList, SelectListLayoutOptions, SelectListTheme,
};
use crate::image_markers::{
    format_image_marker, image_marker_id, is_image_marker, EditorImageState, ImageMarkerCanonicalization,
    ImageMarkerRegistry, IMAGE_MARKER_REGEX,
};
use crate::keybindings::get_keybindings;
use crate::keys::{decode_printable_key, matches_key};
use crate::kill_ring::KillRing;
use crate::paste_markers::{
    grapheme_segment_refs, is_paste_marker, paste_marker_id, segment_with_markers, word_segment_refs,
    EditorPasteState, MarkerSegment, PasteMarkerRegistry, SegmentRef,
};
use crate::process_env::Env;
use crate::terminal::normalize_warp_wsl_shift_enter_input;
use crate::tui::{Component, Focusable, TuiMouseEvent, TuiMouseEventResult, CURSOR_MARKER};
use crate::undo_stack::UndoStack;
use crate::utils::{
    graphemes, is_cjk_break, is_whitespace_char, slice_by_column, visible_width, word_segments, WordSegment,
};
use crate::word_navigation::{find_word_backward, find_word_forward, WordNavigationOptions};

static TRIGGER_CHARACTER_CLASS_ESCAPE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[\\^$.*+?()\[\]{}|-]").expect("valid escape regex"));

/// Which atomic marker family a segment belongs to, if any.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkerKind {
    Paste,
    Image,
}

/// Atomic-marker predicate covering every marker family the editor treats as one grapheme.
pub fn is_atomic_marker(segment: &str) -> bool {
    is_paste_marker(segment) || is_image_marker(segment)
}

pub fn marker_kind(segment: &str) -> Option<MarkerKind> {
    if is_paste_marker(segment) {
        return Some(MarkerKind::Paste);
    }
    if is_image_marker(segment) {
        return Some(MarkerKind::Image);
    }
    None
}

fn contains_marker_prefix(text: &str) -> bool {
    text.contains("[paste #") || text.contains("[Image #")
}

/// Represents a chunk of text for word-wrap layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextChunk {
    pub text: String,
    pub start_index: usize,
    pub end_index: usize,
}

#[derive(Debug, Clone)]
struct WrappedLine {
    chunks: Vec<TextChunk>,
    width: usize,
}

#[derive(Debug, Clone)]
struct CachedWrappedLine {
    chunks: Vec<TextChunk>,
    width: usize,
    line_ref: String,
    content_width: usize,
}

fn is_printable_ascii_text(text: &str) -> bool {
    text.bytes().all(|byte| (0x20..=0x7e).contains(&byte))
}

fn word_wrap_ascii_line(line: &str, max_width: usize) -> Vec<TextChunk> {
    if line.chars().count() <= max_width {
        return vec![TextChunk {
            text: line.to_string(),
            start_index: 0,
            end_index: line.len(),
        }];
    }

    let indices: Vec<usize> = line.char_indices().map(|(index, _)| index).collect();
    let char_count = indices.len();
    let char_at = |index: usize| -> usize { indices[index] };
    let byte_at = |index: usize| -> usize {
        if index < char_count {
            indices[index]
        } else {
            line.len()
        }
    };

    let mut chunks: Vec<TextChunk> = Vec::new();
    let mut chunk_start = 0usize;
    while chunk_start < char_count {
        let mut chunk_end = char_count.min(chunk_start + max_width);
        if chunk_end < char_count {
            let mut break_at: Option<usize> = None;
            let mut i = chunk_end;
            while i > chunk_start {
                let ch = line[char_at(i - 1)..].chars().next().unwrap_or(' ');
                if ch == ' ' || ch == '\t' {
                    break_at = Some(i - 1);
                    break;
                }
                i -= 1;
            }
            if let Some(index) = break_at
                && index > chunk_start
            {
                chunk_end = index;
            }
        }

        let raw = &line[byte_at(chunk_start)..byte_at(chunk_end)];
        let text = raw.trim_end_matches([' ', '\t']);
        if !text.is_empty() || chunks.is_empty() {
            chunks.push(TextChunk {
                text: text.to_string(),
                start_index: byte_at(chunk_start),
                end_index: byte_at(chunk_start) + raw.len(),
            });
        }
        chunk_start = chunk_end;
        while chunk_start < char_count {
            let ch = line[char_at(chunk_start)..].chars().next().unwrap_or(' ');
            if ch != ' ' && ch != '\t' {
                break;
            }
            chunk_start += 1;
        }
    }

    if chunks.is_empty() {
        vec![TextChunk {
            text: String::new(),
            start_index: 0,
            end_index: 0,
        }]
    } else {
        chunks
    }
}

/// Split a line into word-wrapped chunks, wrapping at word boundaries when possible and falling
/// back to character-level wrapping for words longer than the available width.
pub fn word_wrap_line(line: &str, max_width: usize, pre_segmented: Option<&[MarkerSegment]>) -> Vec<TextChunk> {
    if line.is_empty() || max_width == 0 {
        return vec![TextChunk {
            text: String::new(),
            start_index: 0,
            end_index: 0,
        }];
    }
    if pre_segmented.is_none()
        && !line.chars().any(|c| c.is_whitespace())
        && !contains_marker_prefix(line)
        && is_printable_ascii_text(line)
    {
        return word_wrap_ascii_line(line, max_width);
    }

    let line_width = visible_width(line);
    if line_width <= max_width {
        return vec![TextChunk {
            text: line.to_string(),
            start_index: 0,
            end_index: line.len(),
        }];
    }

    let owned_segments: Vec<MarkerSegment>;
    let segments: &[MarkerSegment] = match pre_segmented {
        Some(segments) => segments,
        None => {
            owned_segments = grapheme_segment_refs(line)
                .into_iter()
                .map(|segment| MarkerSegment {
                    index: segment.index,
                    segment: segment.segment.to_string(),
                })
                .collect();
            &owned_segments
        }
    };

    let mut chunks: Vec<TextChunk> = Vec::new();
    let mut current_width = 0usize;
    let mut chunk_start = 0usize;
    let mut wrap_opp_index: Option<usize> = None;
    let mut wrap_opp_width = 0usize;

    for i in 0..segments.len() {
        let grapheme = segments[i].segment.clone();
        let g_width = visible_width(&grapheme);
        let char_index = segments[i].index;
        let is_ws = !is_atomic_marker(&grapheme) && is_whitespace_char(&grapheme);

        if current_width + g_width > max_width {
            if let Some(opp) = wrap_opp_index {
                if current_width - wrap_opp_width + g_width <= max_width {
                    chunks.push(TextChunk {
                        text: line[chunk_start..opp].to_string(),
                        start_index: chunk_start,
                        end_index: opp,
                    });
                    chunk_start = opp;
                    current_width -= wrap_opp_width;
                }
            } else if chunk_start < char_index {
                chunks.push(TextChunk {
                    text: line[chunk_start..char_index].to_string(),
                    start_index: chunk_start,
                    end_index: char_index,
                });
                chunk_start = char_index;
                current_width = 0;
            }
            wrap_opp_index = None;
        }

        if g_width > max_width {
            let sub_chunks = word_wrap_line(&grapheme, max_width, None);
            for sub in &sub_chunks[..sub_chunks.len().saturating_sub(1)] {
                chunks.push(TextChunk {
                    text: sub.text.clone(),
                    start_index: char_index + sub.start_index,
                    end_index: char_index + sub.end_index,
                });
            }
            let last = sub_chunks.last().cloned().unwrap_or(TextChunk {
                text: String::new(),
                start_index: 0,
                end_index: 0,
            });
            chunk_start = char_index + last.start_index;
            current_width = visible_width(&last.text);
            wrap_opp_index = None;
            continue;
        }

        current_width += g_width;

        let next = segments.get(i + 1);
        if is_ws {
            if let Some(next) = next
                && (is_atomic_marker(&next.segment) || !is_whitespace_char(&next.segment))
            {
                wrap_opp_index = Some(next.index);
                wrap_opp_width = current_width;
            }
        } else if let Some(next) = next
            && !is_whitespace_char(&next.segment)
        {
            let is_cjk = !is_atomic_marker(&grapheme) && is_cjk_break(&grapheme);
            let next_is_cjk = !is_atomic_marker(&next.segment) && is_cjk_break(&next.segment);
            if is_cjk || next_is_cjk {
                wrap_opp_index = Some(next.index);
                wrap_opp_width = current_width;
            }
        }
    }

    chunks.push(TextChunk {
        text: line[chunk_start..].to_string(),
        start_index: chunk_start,
        end_index: line.len(),
    });

    chunks
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorState {
    pub lines: Vec<String>,
    pub cursor_line: usize,
    pub cursor_col: usize,
}

impl Default for EditorState {
    fn default() -> Self {
        Self {
            lines: vec![String::new()],
            cursor_line: 0,
            cursor_col: 0,
        }
    }
}

/// Undo snapshot: editor text state plus the paste and image registries.
#[derive(Clone)]
pub struct EditorSnapshot {
    pub state: EditorState,
    pub paste_state: EditorPasteState,
    pub image_state: EditorImageState,
    pub attachment_state: Option<AttachmentState>,
}

/// Opaque attachment payload snapshot captured from the owner, restored on undo.
pub type AttachmentState = Rc<dyn std::any::Any>;

#[derive(Debug, Clone, PartialEq, Eq)]
struct LayoutLine {
    text: String,
    has_cursor: bool,
    cursor_pos: Option<usize>,
    logical_line: usize,
    start_index: usize,
}

pub type StyleFn = Rc<dyn Fn(&str) -> String>;
/// senpi's `terminalSocketExists` option.
pub type TerminalSocketExists = Rc<dyn Fn(&str) -> bool>;
pub type SubmitCallback = Box<dyn FnMut(&str)>;
pub type ChangeCallback = Box<dyn FnMut(&str)>;
pub type ImageMarkersChangedCallback = Box<dyn FnMut(&[u64])>;
pub type SnapshotAttachmentCallback = Box<dyn Fn() -> Option<AttachmentState>>;
pub type RestoreAttachmentCallback = Box<dyn FnMut(&AttachmentState)>;

pub struct EditorTheme {
    pub border_color: StyleFn,
    /// Style for resolved mention tokens (for example a known `$skill`); omit to render them plain.
    pub mention: Option<StyleFn>,
    /// Theme for the autocomplete/slash SelectList built by `create_autocomplete_list()`.
    pub select_list: SelectListTheme,
}

/// Host seam: senpi's editor reaches `this.tui.terminal.rows` and `this.tui.requestRender()`.
pub trait EditorTuiHost {
    fn request_render(&self);
    fn terminal_rows(&self) -> usize;
}

#[derive(Default)]
pub struct EditorOptions {
    pub padding_x: Option<usize>,
    pub autocomplete_max_visible: Option<usize>,
    pub terminal_environment: Option<Env>,
    pub terminal_platform: Option<String>,
    pub terminal_socket_exists: Option<TerminalSocketExists>,
}

const SLASH_COMMAND_MIN_PRIMARY_COLUMN_WIDTH: usize = 12;
const SLASH_COMMAND_MAX_PRIMARY_COLUMN_WIDTH: usize = 32;
const DEFAULT_AUTOCOMPLETE_TRIGGER_CHARACTERS: [&str; 3] = ["@", "#", "$"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AutocompleteState {
    Regular,
    Force,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LastAction {
    Kill,
    Yank,
    TypeWord,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JumpMode {
    Forward,
    Backward,
}

fn escape_character_class(value: &str) -> String {
    TRIGGER_CHARACTER_CLASS_ESCAPE.replace_all(value, "\\$0").into_owned()
}

fn build_trigger_pattern(trigger_characters: &[String]) -> Regex {
    let class: String = trigger_characters
        .iter()
        .map(|character| escape_character_class(character))
        .collect();
    Regex::new(&format!("(?:^|[\\s])[{class}][^\\s]*$")).expect("valid trigger pattern")
}

fn build_debounce_pattern(trigger_characters: &[String]) -> Regex {
    let escaped_without_at: String = trigger_characters
        .iter()
        .filter(|character| character.as_str() != "@")
        .map(|character| escape_character_class(character))
        .collect();
    Regex::new(&format!(
        "(?:^|[ \\t])(?:@(?:\"[^\"]*|[^\\s]*)|[{escaped_without_at}][^\\s]*)$"
    ))
    .expect("valid debounce pattern")
}

fn create_scroll_border(direction: &str, hidden_line_count: usize, width: usize) -> String {
    let available_width = width;
    let label = format!(" {direction} {hidden_line_count} more ");
    let label_width = visible_width(&label);
    if label_width + 2 <= available_width {
        let left_width = (available_width - label_width) / 2;
        return format!(
            "{}{}{}",
            "─".repeat(left_width),
            label,
            "─".repeat(available_width - left_width - label_width)
        );
    }

    let indicator = format!("─── {direction} {hidden_line_count} more ");
    let indicator_width = visible_width(&indicator);
    if available_width >= indicator_width {
        return format!("{indicator}{}", "─".repeat(available_width - indicator_width));
    }

    let ellipsis: String = "...".chars().take(available_width).collect();
    let indicator_width = available_width - visible_width(&ellipsis);
    format!(
        "{}{}",
        slice_by_column(&indicator, 0, indicator_width, true),
        ellipsis
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SegmentMode {
    Word,
    Grapheme,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct VisualLine {
    logical_line: usize,
    start_col: usize,
    length: usize,
}

/// Multiline editor primitive.
pub struct Editor {
    tui: Rc<dyn EditorTuiHost>,
    theme: EditorTheme,
    padding_x: usize,
    terminal_environment: Env,
    terminal_platform: Option<String>,
    terminal_socket_exists: Option<TerminalSocketExists>,
    state: EditorState,
    focused: bool,
    last_width: usize,
    rendered_visible_line_count: usize,
    rendered_autocomplete_height: usize,
    scroll_offset: usize,
    pub border_color: StyleFn,
    autocomplete_provider: Option<Rc<RefCell<dyn AutocompleteProvider>>>,
    autocomplete_trigger_characters: Vec<String>,
    autocomplete_trigger_pattern: Regex,
    autocomplete_debounce_pattern: Regex,
    autocomplete_list: Option<SelectList>,
    autocomplete_state: Option<AutocompleteState>,
    autocomplete_prefix: String,
    autocomplete_max_visible: usize,
    pending_autocomplete_select: Rc<RefCell<Option<SelectItem>>>,
    paste_markers: PasteMarkerRegistry,
    image_markers: ImageMarkerRegistry,
    paste_buffer: String,
    is_in_paste: bool,
    history: Vec<String>,
    history_index: i64,
    history_draft: Option<EditorState>,
    kill_ring: KillRing,
    last_action: Option<LastAction>,
    jump_mode: Option<JumpMode>,
    preferred_visual_col: Option<usize>,
    snapped_from_cursor_col: Option<usize>,
    undo_stack: UndoStack<EditorSnapshot>,
    wrapped_line_cache: Vec<Option<CachedWrappedLine>>,
    pub on_submit: Option<SubmitCallback>,
    pub on_change: Option<ChangeCallback>,
    /// Fired whenever image markers are added, removed, pruned or renumbered, with the
    /// PRE-renumber ids in reading order.
    pub on_image_markers_changed: Option<ImageMarkersChangedCallback>,
    /// Owner hook: capture the attachment payloads keyed by marker id so an undo can restore them.
    pub snapshot_attachment_state: Option<SnapshotAttachmentCallback>,
    /// Owner hook: restore the attachment payloads captured by `snapshot_attachment_state`.
    pub restore_attachment_state: Option<RestoreAttachmentCallback>,
    pub disable_submit: bool,
}

impl Editor {
    pub fn new(tui: Rc<dyn EditorTuiHost>, theme: EditorTheme, options: EditorOptions) -> Self {
        let padding_x = options.padding_x.unwrap_or(0);
        let autocomplete_max_visible = options.autocomplete_max_visible.unwrap_or(5).clamp(3, 20);
        let trigger_characters: Vec<String> = DEFAULT_AUTOCOMPLETE_TRIGGER_CHARACTERS
            .iter()
            .map(|character| (*character).to_string())
            .collect();
        Self {
            tui,
            border_color: theme.border_color.clone(),
            theme,
            padding_x,
            terminal_environment: options.terminal_environment.unwrap_or_default(),
            terminal_platform: options.terminal_platform,
            terminal_socket_exists: options.terminal_socket_exists,
            state: EditorState::default(),
            focused: false,
            last_width: 80,
            rendered_visible_line_count: 1,
            rendered_autocomplete_height: 0,
            scroll_offset: 0,
            autocomplete_provider: None,
            autocomplete_trigger_pattern: build_trigger_pattern(&trigger_characters),
            autocomplete_debounce_pattern: build_debounce_pattern(&trigger_characters),
            autocomplete_trigger_characters: trigger_characters,
            autocomplete_list: None,
            autocomplete_state: None,
            autocomplete_prefix: String::new(),
            autocomplete_max_visible,
            pending_autocomplete_select: Rc::new(RefCell::new(None)),
            paste_markers: PasteMarkerRegistry::new(),
            image_markers: ImageMarkerRegistry::new(),
            paste_buffer: String::new(),
            is_in_paste: false,
            history: Vec::new(),
            history_index: -1,
            history_draft: None,
            kill_ring: KillRing::new(),
            last_action: None,
            jump_mode: None,
            preferred_visual_col: None,
            snapped_from_cursor_col: None,
            undo_stack: UndoStack::new(),
            wrapped_line_cache: Vec::new(),
            on_submit: None,
            on_change: None,
            on_image_markers_changed: None,
            snapshot_attachment_state: None,
            restore_attachment_state: None,
            disable_submit: false,
        }
    }

    /// senpi's `this.segment(text, "word")`: marker-aware word segments for word navigation.
    fn segment_word<'a>(&self, text: &'a str) -> Vec<WordSegment<'a>> {
        let merged = self.segment(text, SegmentMode::Word);
        let base = word_segments(text);
        let mut out: Vec<WordSegment<'a>> = Vec::with_capacity(merged.len());
        let mut base_index = 0usize;
        for segment in merged {
            while base_index < base.len() && base[base_index].index < segment.index {
                base_index += 1;
            }
            let is_word_like = match base.get(base_index) {
                Some(candidate)
                    if candidate.index == segment.index
                        && candidate.segment.len() == segment.segment.len() =>
                {
                    candidate.is_word_like
                }
                _ => true,
            };
            out.push(WordSegment {
                segment: &text[segment.index..segment.index + segment.segment.len()],
                index: segment.index,
                is_word_like,
            });
        }
        out
    }

    /// Segment text with marker awareness, only merging exact canonical markers.
    fn segment(&self, text: &str, mode: SegmentMode) -> Vec<MarkerSegment> {
        let authorized = self.authorized_markers(text);
        let base = match mode {
            SegmentMode::Word => word_segment_refs(text),
            SegmentMode::Grapheme => grapheme_segment_refs(text),
        };
        segment_with_markers(text, &base, &authorized)
    }

    /// Union of every marker family that must segment as one atomic grapheme.
    fn authorized_markers(&self, text: &str) -> BTreeSet<String> {
        let image_markers = self.image_markers.authorized_markers(text);
        if image_markers.is_empty() {
            return self.paste_markers.authorized_markers(text);
        }
        let mut merged = self.paste_markers.authorized_markers(text);
        for marker in image_markers {
            merged.insert(marker);
        }
        merged
    }

    fn remove_paste_marker(&mut self, id: u64) -> bool {
        let removal = self.paste_markers.remove(id, &self.get_text());
        if !removal.removed {
            return false;
        }
        self.state.lines = split_lines(&removal.text);
        true
    }

    fn remove_image_marker(&mut self, id: u64) -> bool {
        let survivors: Vec<u64> = self
            .image_markers
            .ids(&self.get_text())
            .into_iter()
            .filter(|entry_id| *entry_id != id)
            .collect();
        let removal = self.image_markers.remove(id, &self.get_text());
        if !removal.removed {
            return false;
        }
        self.state.lines = split_lines(&removal.text);
        self.notify_image_markers_changed_order(&survivors);
        true
    }

    fn remove_marker_segment(&mut self, segment: &str) -> bool {
        match marker_kind(segment) {
            Some(MarkerKind::Paste) => match paste_marker_id(segment) {
                Some(id) => self.remove_paste_marker(id),
                None => false,
            },
            Some(MarkerKind::Image) => match image_marker_id(segment) {
                Some(id) => self.remove_image_marker(id),
                None => false,
            },
            None => false,
        }
    }

    fn notify_image_markers_changed(&mut self) {
        let order = self.image_markers.ids(&self.get_text());
        self.notify_image_markers_changed_order(&order);
    }

    fn notify_image_markers_changed_order(&mut self, order: &[u64]) {
        if let Some(callback) = self.on_image_markers_changed.as_mut() {
            callback(order);
        }
    }

    fn notify_change(&mut self) {
        let text = self.get_text();
        if let Some(callback) = self.on_change.as_mut() {
            callback(&text);
        }
    }

    pub fn get_padding_x(&self) -> usize {
        self.padding_x
    }

    pub fn set_padding_x(&mut self, padding: usize) {
        if self.padding_x != padding {
            self.padding_x = padding;
            self.tui.request_render();
        }
    }

    pub fn get_autocomplete_max_visible(&self) -> usize {
        self.autocomplete_max_visible
    }

    pub fn set_autocomplete_max_visible(&mut self, max_visible: usize) {
        let new_max_visible = max_visible.clamp(3, 20);
        if self.autocomplete_max_visible != new_max_visible {
            self.autocomplete_max_visible = new_max_visible;
            self.tui.request_render();
        }
    }

    pub fn set_autocomplete_provider(&mut self, provider: Rc<RefCell<dyn AutocompleteProvider>>) {
        self.cancel_autocomplete();
        let trigger_characters = provider.borrow().trigger_characters();
        self.autocomplete_provider = Some(provider);
        self.set_autocomplete_trigger_characters(&trigger_characters);
    }

    /// Add a prompt to history for up/down arrow navigation. Called after successful submission.
    pub fn add_to_history(&mut self, text: &str) {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return;
        }
        if self.history.first().is_some_and(|first| first == trimmed) {
            return;
        }
        self.history.insert(0, trimmed.to_string());
        if self.history.len() > 100 {
            self.history.pop();
        }
    }

    fn is_editor_empty(&self) -> bool {
        self.state.lines.len() == 1 && self.state.lines[0].is_empty()
    }

    fn is_on_first_visual_line(&mut self) -> bool {
        let visual_lines = self.build_visual_line_map(self.last_width);
        self.find_current_visual_line(&visual_lines) == 0
    }

    fn is_on_last_visual_line(&mut self) -> bool {
        let visual_lines = self.build_visual_line_map(self.last_width);
        self.find_current_visual_line(&visual_lines) == visual_lines.len().saturating_sub(1)
    }

    fn navigate_history(&mut self, direction: i64) {
        self.last_action = None;
        if self.history.is_empty() {
            return;
        }

        let new_index = self.history_index - direction;
        if new_index < -1 || new_index >= self.history.len() as i64 {
            return;
        }

        if self.history_index == -1 && new_index >= 0 {
            self.push_undo_snapshot();
            self.history_draft = Some(self.state.clone());
        }

        self.history_index = new_index;

        if self.history_index == -1 {
            let draft = self.history_draft.take();
            if let Some(draft) = draft {
                self.state = draft;
                self.preferred_visual_col = None;
                self.snapped_from_cursor_col = None;
                self.scroll_offset = 0;
                self.notify_change();
            } else {
                self.set_text_internal("", CursorPlacement::End);
            }
        } else {
            let entry = self
                .history
                .get(self.history_index as usize)
                .cloned()
                .unwrap_or_default();
            let placement = if direction == -1 {
                CursorPlacement::Start
            } else {
                CursorPlacement::End
            };
            self.set_text_internal(&entry, placement);
        }
    }

    fn exit_history_browsing(&mut self) {
        self.history_index = -1;
        self.history_draft = None;
    }

    /// Internal set_text that doesn't reset history state - used by `navigate_history`.
    fn set_text_internal(&mut self, text: &str, cursor_placement: CursorPlacement) {
        let lines = split_lines(text);
        self.state.lines = if lines.is_empty() {
            vec![String::new()]
        } else {
            lines
        };
        self.state.cursor_line = match cursor_placement {
            CursorPlacement::Start => 0,
            CursorPlacement::End => self.state.lines.len().saturating_sub(1),
        };
        let col = match cursor_placement {
            CursorPlacement::Start => 0,
            CursorPlacement::End => self
                .state
                .lines
                .get(self.state.cursor_line)
                .map(String::len)
                .unwrap_or(0),
        };
        self.set_cursor_col(col);
        self.scroll_offset = 0;
        self.notify_change();
    }

    fn get_wrapped_line(&mut self, line_index: usize, content_width: usize) -> WrappedLine {
        let line = self
            .state
            .lines
            .get(line_index)
            .cloned()
            .unwrap_or_default();
        if let Some(Some(cached)) = self.wrapped_line_cache.get(line_index)
            && cached.line_ref == line
            && cached.content_width == content_width
        {
            return WrappedLine {
                chunks: cached.chunks.clone(),
                width: cached.width,
            };
        }

        let width = if is_printable_ascii_text(&line) {
            line.len()
        } else {
            visible_width(&line)
        };
        if contains_marker_prefix(&line) {
            let chunks = if width <= content_width {
                vec![TextChunk {
                    text: line.clone(),
                    start_index: 0,
                    end_index: line.len(),
                }]
            } else {
                let segments = self.segment(&line, SegmentMode::Grapheme);
                word_wrap_line(&line, content_width, Some(&segments))
            };
            return WrappedLine { chunks, width };
        }

        let chunks = if width <= content_width {
            vec![TextChunk {
                text: line.clone(),
                start_index: 0,
                end_index: line.len(),
            }]
        } else {
            word_wrap_line(&line, content_width, None)
        };
        let wrapped = CachedWrappedLine {
            chunks: chunks.clone(),
            width,
            line_ref: line,
            content_width,
        };
        while self.wrapped_line_cache.len() <= line_index {
            self.wrapped_line_cache.push(None);
        }
        self.wrapped_line_cache[line_index] = Some(wrapped);
        WrappedLine { chunks, width }
    }

    pub fn render_top_border(&self, width: usize, hidden_line_count: usize) -> String {
        let border = if hidden_line_count > 0 {
            create_scroll_border("↑", hidden_line_count, width)
        } else {
            "─".repeat(width)
        };
        (self.border_color)(&border)
    }

    pub fn render_bottom_border(&self, width: usize, hidden_line_count: usize) -> String {
        let border = if hidden_line_count > 0 {
            create_scroll_border("↓", hidden_line_count, width)
        } else {
            "─".repeat(width)
        };
        (self.border_color)(&border)
    }

    pub fn render_editor(&mut self, width: usize) -> Vec<String> {
        let max_padding = (width.saturating_sub(1)) / 2;
        let padding_x = self.padding_x.min(max_padding);
        let content_width = width.saturating_sub(padding_x * 2).max(1);
        let layout_width = content_width.saturating_sub(if padding_x > 0 { 0 } else { 1 }).max(1);

        self.last_width = layout_width;

        let layout_lines = self.layout_text(layout_width);

        let terminal_rows = self.tui.terminal_rows();
        let max_visible_lines = ((terminal_rows as f64 * 0.3).floor() as usize).max(5);

        let cursor_line_index = layout_lines
            .iter()
            .position(|line| line.has_cursor)
            .unwrap_or(0);

        if cursor_line_index < self.scroll_offset {
            self.scroll_offset = cursor_line_index;
        } else if cursor_line_index >= self.scroll_offset + max_visible_lines {
            self.scroll_offset = cursor_line_index - max_visible_lines + 1;
        }

        let max_scroll_offset = layout_lines.len().saturating_sub(max_visible_lines);
        self.scroll_offset = self.scroll_offset.min(max_scroll_offset);

        let end = (self.scroll_offset + max_visible_lines).min(layout_lines.len());
        let visible_lines: Vec<LayoutLine> = if self.scroll_offset <= end {
            layout_lines[self.scroll_offset..end].to_vec()
        } else {
            Vec::new()
        };
        self.rendered_visible_line_count = visible_lines.len();

        let mut result: Vec<String> = Vec::new();
        let left_padding = " ".repeat(padding_x);
        let right_padding = left_padding.clone();

        result.push(self.render_top_border(width, self.scroll_offset));

        let emit_cursor_marker = self.focused;
        let draw_fake_cursor = true;

        let mut mention_ranges_by_line: BTreeMap<usize, Vec<MentionRange>> = BTreeMap::new();
        let mention_style = self.theme.mention.clone();

        for layout_line in &visible_lines {
            let mut line_visible_width = visible_width(&layout_line.text);
            let mut cursor_in_padding = false;

            let mention_ranges: Vec<MentionRange> = if mention_style.is_none() {
                Vec::new()
            } else {
                match mention_ranges_by_line.get(&layout_line.logical_line) {
                    Some(ranges) => ranges.clone(),
                    None => {
                        let logical_line = self
                            .state
                            .lines
                            .get(layout_line.logical_line)
                            .cloned()
                            .unwrap_or_default();
                        let ranges: Vec<MentionRange> = match &self.autocomplete_provider {
                            Some(provider) => provider.borrow().get_mention_ranges(&logical_line),
                            None => Vec::new(),
                        };
                        mention_ranges_by_line.insert(layout_line.logical_line, ranges.clone());
                        ranges
                    }
                }
            };
            let mention_ranges: Vec<MentionRange> = mention_ranges
                .into_iter()
                .map(|range| MentionRange {
                    start: range.start.saturating_sub(layout_line.start_index),
                    end: range.end.saturating_sub(layout_line.start_index),
                })
                .collect();

            let cursor = match (layout_line.has_cursor, layout_line.cursor_pos) {
                (true, Some(cursor_pos)) => Some(EditorLineCursor {
                    pos: cursor_pos,
                    marker: if emit_cursor_marker {
                        CURSOR_MARKER.to_string()
                    } else {
                        String::new()
                    },
                    draw_fake_cursor,
                }),
                _ => None,
            };
            let mention_style_ref: Rc<dyn Fn(&str) -> String> = mention_style
                .clone()
                .unwrap_or_else(|| Rc::new(|text: &str| text.to_string()));
            let rendered = render_editor_line(EditorLineRenderInput {
                text: &layout_line.text,
                mentions: &mention_ranges,
                mention_style: mention_style_ref.as_ref(),
                cursor,
            });
            let display_text = rendered.text;
            if rendered.cursor_appended {
                line_visible_width += 1;
                if line_visible_width > content_width && padding_x > 0 {
                    cursor_in_padding = true;
                }
            }

            let padding = " ".repeat(content_width.saturating_sub(line_visible_width));
            let line_right_padding = if cursor_in_padding {
                right_padding.get(1..).unwrap_or("").to_string()
            } else {
                right_padding.clone()
            };

            result.push(format!(
                "{left_padding}{display_text}{padding}{line_right_padding}"
            ));
        }

        let lines_below = layout_lines.len().saturating_sub(self.scroll_offset + visible_lines.len());
        result.push(self.render_bottom_border(width, lines_below));

        self.rendered_autocomplete_height = 0;
        if self.autocomplete_state.is_some()
            && let Some(list) = self.autocomplete_list.as_mut()
        {
            let autocomplete_result = list.render(content_width);
            self.rendered_autocomplete_height = autocomplete_result.len();
            for line in autocomplete_result {
                let line_width = visible_width(&line);
                let line_padding = " ".repeat(content_width.saturating_sub(line_width));
                result.push(format!("{left_padding}{line}{line_padding}{right_padding}"));
            }
        }

        result
    }

    pub fn handle_editor_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        let autocomplete_start_row = self.rendered_visible_line_count + 2;
        if self.autocomplete_state.is_some() && self.autocomplete_list.is_some() {
            let event_y = event.y.max(0) as usize;
            if event_y >= autocomplete_start_row
                && event_y < autocomplete_start_row + self.rendered_autocomplete_height
            {
                let max_padding = (event.width.saturating_sub(1)) / 2;
                let padding_x = self.padding_x.min(max_padding);
                let content_width = event.width.saturating_sub(padding_x * 2).max(1);
                let adjusted = TuiMouseEvent {
                    x: event.x - padding_x as i64,
                    y: event.y - autocomplete_start_row as i64,
                    width: content_width,
                    height: self.rendered_autocomplete_height,
                    ..*event
                };
                let result = self
                    .autocomplete_list
                    .as_mut()
                    .and_then(|list| list.handle_mouse(&adjusted));
                self.drain_pending_autocomplete_select();
                return result.map(|mut result| {
                    result.focus = true;
                    result
                });
            }
        }

        if event.event_type != crate::tui::TuiMouseEventType::Click
            || event.button != crate::tui::TuiMouseButton::Left
        {
            return None;
        }
        if event.y <= 0 || event.y as usize > self.rendered_visible_line_count {
            return Some(TuiMouseEventResult {
                handled: true,
                focus: true,
                ..TuiMouseEventResult::default()
            });
        }

        let visual_lines = self.build_visual_line_map(self.last_width);
        let visual_line_index = self.scroll_offset + (event.y as usize) - 1;
        let Some(visual_line) = visual_lines.get(visual_line_index).copied() else {
            return Some(TuiMouseEventResult {
                handled: true,
                focus: true,
                ..TuiMouseEventResult::default()
            });
        };
        let logical_line = self
            .state
            .lines
            .get(visual_line.logical_line)
            .cloned()
            .unwrap_or_default();
        let chunk_end = visual_line.start_col + visual_line.length;
        let chunk = logical_line
            .get(visual_line.start_col..chunk_end)
            .unwrap_or("")
            .to_string();
        let max_padding = (event.width.saturating_sub(1)) / 2;
        let padding_x = self.padding_x.min(max_padding);
        let target_column = (event.x - padding_x as i64).max(0) as usize;
        let mut visible_column = 0usize;
        let mut target_index = chunk.len();
        let mut last_grapheme_index = 0usize;
        for grapheme in self.segment(&chunk, SegmentMode::Grapheme) {
            let next_column = visible_column + visible_width(&grapheme.segment);
            last_grapheme_index = grapheme.index;
            if target_column < next_column {
                target_index = grapheme.index;
                break;
            }
            visible_column = next_column;
        }
        let is_last_segment = visual_line_index == visual_lines.len().saturating_sub(1)
            || visual_lines
                .get(visual_line_index + 1)
                .map(|next| next.logical_line != visual_line.logical_line)
                .unwrap_or(true);
        if !is_last_segment && target_index == chunk.len() && !chunk.is_empty() {
            target_index = last_grapheme_index;
        }

        self.state.cursor_line = visual_line.logical_line;
        self.set_cursor_col(visual_line.start_col + target_index);
        self.last_action = None;
        self.exit_history_browsing();
        if self.autocomplete_state.is_some() {
            self.update_autocomplete();
        }
        Some(TuiMouseEventResult {
            handled: true,
            focus: true,
            ..TuiMouseEventResult::default()
        })
    }

    fn layout_text(&mut self, content_width: usize) -> Vec<LayoutLine> {
        let mut layout_lines: Vec<LayoutLine> = Vec::new();

        if self.state.lines.is_empty()
            || (self.state.lines.len() == 1 && self.state.lines[0].is_empty())
        {
            self.wrapped_line_cache.truncate(self.state.lines.len());
            layout_lines.push(LayoutLine {
                text: String::new(),
                has_cursor: true,
                cursor_pos: Some(0),
                logical_line: 0,
                start_index: 0,
            });
            return layout_lines;
        }

        for i in 0..self.state.lines.len() {
            let line = self.state.lines.get(i).cloned().unwrap_or_default();
            let is_current_line = i == self.state.cursor_line;
            let wrapped_line = self.get_wrapped_line(i, content_width);

            if wrapped_line.width <= content_width {
                if is_current_line {
                    layout_lines.push(LayoutLine {
                        text: line,
                        has_cursor: true,
                        cursor_pos: Some(self.state.cursor_col),
                        logical_line: i,
                        start_index: 0,
                    });
                } else {
                    layout_lines.push(LayoutLine {
                        text: line,
                        has_cursor: false,
                        cursor_pos: None,
                        logical_line: i,
                        start_index: 0,
                    });
                }
            } else {
                let chunks = wrapped_line.chunks;
                for (chunk_index, chunk) in chunks.iter().enumerate() {
                    let cursor_pos = self.state.cursor_col;
                    let is_last_chunk = chunk_index == chunks.len().saturating_sub(1);

                    let mut has_cursor_in_chunk = false;
                    let mut adjusted_cursor_pos = 0usize;

                    if is_current_line {
                        if is_last_chunk {
                            has_cursor_in_chunk = cursor_pos >= chunk.start_index;
                            adjusted_cursor_pos = cursor_pos.saturating_sub(chunk.start_index);
                        } else {
                            has_cursor_in_chunk =
                                cursor_pos >= chunk.start_index && cursor_pos < chunk.end_index;
                            if has_cursor_in_chunk {
                                adjusted_cursor_pos = cursor_pos - chunk.start_index;
                                if adjusted_cursor_pos > chunk.text.len() {
                                    adjusted_cursor_pos = chunk.text.len();
                                }
                            }
                        }
                    }

                    if has_cursor_in_chunk {
                        layout_lines.push(LayoutLine {
                            text: chunk.text.clone(),
                            has_cursor: true,
                            cursor_pos: Some(adjusted_cursor_pos),
                            logical_line: i,
                            start_index: chunk.start_index,
                        });
                    } else {
                        layout_lines.push(LayoutLine {
                            text: chunk.text.clone(),
                            has_cursor: false,
                            cursor_pos: None,
                            logical_line: i,
                            start_index: chunk.start_index,
                        });
                    }
                }
            }
        }

        self.wrapped_line_cache.truncate(self.state.lines.len());
        layout_lines
    }

    pub fn get_text(&self) -> String {
        self.state.lines.join("\n")
    }

    /// Get text with paste markers expanded to their actual content.
    pub fn get_expanded_text(&self) -> String {
        self.paste_markers.expand(&self.state.lines.join("\n"))
    }

    pub fn get_lines(&self) -> Vec<String> {
        self.state.lines.clone()
    }

    pub fn get_cursor(&self) -> (usize, usize) {
        (self.state.cursor_line, self.state.cursor_col)
    }

    pub fn set_text(&mut self, text: &str) {
        self.cancel_autocomplete();
        self.last_action = None;
        self.exit_history_browsing();
        let normalized = normalize_text(text);
        let previous_text = self.get_text();
        if previous_text != normalized {
            self.push_undo_snapshot();
        }
        self.paste_markers.prune(&normalized, Some(&previous_text));
        let previous_image_order = self.image_markers.ids(&previous_text);
        self.image_markers.prune(&normalized, Some(&previous_text));
        self.set_text_internal(&normalized, CursorPlacement::End);
        let canonicalization = self.image_markers.canonicalize(&normalized);
        self.apply_canonicalized_text(&canonicalization);
        let image_order = canonicalization.order;
        if previous_image_order.len() != image_order.len()
            || previous_image_order
                .iter()
                .zip(image_order.iter())
                .any(|(left, right)| left != right)
        {
            self.notify_image_markers_changed_order(&image_order);
        }
    }

    /// Snapshot the large-paste registry so it can be transferred to another editor instance.
    pub fn get_paste_state(&self) -> EditorPasteState {
        self.paste_markers.snapshot()
    }

    /// Install a paste registry snapshot taken from another editor instance.
    pub fn set_paste_state(&mut self, state: &EditorPasteState) {
        let text = self.get_text();
        self.paste_markers.install(state, &text);
    }

    /// Snapshot the image-marker registry (ids only, never image payloads).
    pub fn get_image_marker_state(&self) -> EditorImageState {
        self.image_markers.snapshot()
    }

    /// Install an image-marker registry snapshot taken from another editor instance.
    pub fn set_image_marker_state(&mut self, state: &EditorImageState) {
        let text = self.get_text();
        self.image_markers.install(state, &text);
    }

    /// Insert the next canonical `[Image #N]` marker at the cursor and return its id.
    pub fn insert_image_marker(&mut self) -> u64 {
        self.cancel_autocomplete();
        self.push_undo_snapshot();
        self.last_action = None;
        self.exit_history_browsing();
        let marker = self.image_markers.add();
        self.insert_text_at_cursor_internal(&marker);
        let canonicalization = self.image_markers.canonicalize(&self.get_text());
        self.apply_canonicalized_text(&canonicalization);
        let insertion_id = image_marker_id(&marker).unwrap_or(0);
        let final_id = match canonicalization
            .order
            .iter()
            .position(|id| *id == insertion_id)
        {
            Some(index) => index as u64 + 1,
            None => insertion_id,
        };
        self.notify_image_markers_changed_order(&canonicalization.order);
        final_id
    }

    fn apply_canonicalized_text(&mut self, canonicalization: &ImageMarkerCanonicalization) {
        if canonicalization.text == self.get_text() {
            return;
        }
        let old_line = self
            .state
            .lines
            .get(self.state.cursor_line)
            .cloned()
            .unwrap_or_default();
        let cursor_col = self.state.cursor_col;
        let mut renumbered: BTreeMap<u64, u64> = BTreeMap::new();
        for (index, id) in canonicalization.order.iter().enumerate() {
            renumbered.insert(*id, index as u64 + 1);
        }
        let mut delta: i64 = 0;
        for captures in IMAGE_MARKER_REGEX.captures_iter(&old_line) {
            let whole = captures.get(0).expect("group 0");
            let end = whole.end();
            if end > cursor_col {
                break;
            }
            let id = captures
                .get(1)
                .and_then(|m| m.as_str().parse::<u64>().ok())
                .unwrap_or(0);
            let Some(new_id) = renumbered.get(&id) else {
                continue;
            };
            delta += format_image_marker(*new_id).len() as i64 - whole.as_str().len() as i64;
        }
        self.state.lines = split_lines(&canonicalization.text);
        let new_col = (cursor_col as i64 + delta).max(0) as usize;
        self.set_cursor_col(new_col);
    }

    /// Insert text at the current cursor position. Atomic for undo.
    pub fn insert_text_at_cursor(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        self.cancel_autocomplete();
        self.push_undo_snapshot();
        self.last_action = None;
        self.exit_history_browsing();
        self.insert_text_at_cursor_internal(text);
    }

    fn insert_text_at_cursor_internal(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }

        let normalized = normalize_text(text);
        let inserted_lines = split_lines(&normalized);

        let current_line = self
            .state
            .lines
            .get(self.state.cursor_line)
            .cloned()
            .unwrap_or_default();
        let before_cursor = current_line
            .get(..self.state.cursor_col.min(current_line.len()))
            .unwrap_or("")
            .to_string();
        let after_cursor = current_line
            .get(self.state.cursor_col.min(current_line.len())..)
            .unwrap_or("")
            .to_string();

        if inserted_lines.len() == 1 {
            self.state.lines[self.state.cursor_line] = format!("{before_cursor}{normalized}{after_cursor}");
            self.set_cursor_col(self.state.cursor_col + normalized.len());
        } else {
            let mut lines: Vec<String> = Vec::new();
            lines.extend(self.state.lines[..self.state.cursor_line].iter().cloned());
            lines.push(format!("{before_cursor}{}", inserted_lines[0]));
            lines.extend(
                inserted_lines[1..inserted_lines.len().saturating_sub(1)]
                    .iter()
                    .cloned(),
            );
            lines.push(format!(
                "{}{after_cursor}",
                inserted_lines[inserted_lines.len() - 1]
            ));
            lines.extend(self.state.lines[self.state.cursor_line + 1..].iter().cloned());

            self.state.lines = lines;
            self.state.cursor_line += inserted_lines.len() - 1;
            let last_len = inserted_lines
                .last()
                .map(String::len)
                .unwrap_or(0);
            self.set_cursor_col(last_len);
        }

        self.notify_change();
    }

    fn insert_character(&mut self, character: &str, skip_undo_coalescing: bool) {
        self.exit_history_browsing();

        if !skip_undo_coalescing {
            if is_whitespace_char(character) || self.last_action != Some(LastAction::TypeWord) {
                self.push_undo_snapshot();
            }
            self.last_action = Some(LastAction::TypeWord);
        }

        let line = self
            .state
            .lines
            .get(self.state.cursor_line)
            .cloned()
            .unwrap_or_default();

        let cursor_col = self.state.cursor_col.min(line.len());
        let before = line.get(..cursor_col).unwrap_or("").to_string();
        let after = line.get(cursor_col..).unwrap_or("").to_string();

        self.state.lines[self.state.cursor_line] = format!("{before}{character}{after}");
        self.set_cursor_col(self.state.cursor_col + character.len());

        self.notify_change();

        if self.autocomplete_state.is_none() {
            if character == "/" && self.is_at_start_of_message() {
                self.try_trigger_autocomplete(false);
            } else if self
                .autocomplete_trigger_characters
                .iter()
                .any(|trigger| trigger == character)
            {
                let current_line = self
                    .state
                    .lines
                    .get(self.state.cursor_line)
                    .cloned()
                    .unwrap_or_default();
                let text_before_cursor = current_line
                    .get(..self.state.cursor_col.min(current_line.len()))
                    .unwrap_or("")
                    .to_string();
                let char_before_symbol = if text_before_cursor.chars().count() >= 2 {
                    text_before_cursor
                        .chars()
                        .rev()
                        .nth(1)
                        .map(|ch| ch.to_string())
                        .unwrap_or_default()
                } else {
                    String::new()
                };
                if text_before_cursor.chars().count() == 1
                    || char_before_symbol == " "
                    || char_before_symbol == "\t"
                {
                    self.try_trigger_autocomplete(false);
                }
            } else if character
                .chars()
                .next()
                .is_some_and(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_'))
            {
                let current_line = self
                    .state
                    .lines
                    .get(self.state.cursor_line)
                    .cloned()
                    .unwrap_or_default();
                let text_before_cursor = current_line
                    .get(..self.state.cursor_col.min(current_line.len()))
                    .unwrap_or("")
                    .to_string();
                if self.is_in_slash_command_context(&text_before_cursor)
                    || self.autocomplete_trigger_pattern.is_match(&text_before_cursor)
                {
                    self.try_trigger_autocomplete(false);
                }
            }
        } else {
            self.update_autocomplete();
        }
    }

    fn handle_paste(&mut self, pasted_text: &str) {
        self.cancel_autocomplete();
        self.exit_history_browsing();
        self.last_action = None;

        self.push_undo_snapshot();

        let decoded_text = decode_bracketed_paste_ctrl_sequences(pasted_text);
        let clean_text = normalize_text(&decoded_text);

        let mut filtered_text: String = clean_text
            .chars()
            .filter(|ch| *ch == '\n' || (*ch as u32) >= 32)
            .collect();

        if filtered_text.starts_with('/')
            || filtered_text.starts_with('~')
            || filtered_text.starts_with('.')
        {
            let current_line = self
                .state
                .lines
                .get(self.state.cursor_line)
                .cloned()
                .unwrap_or_default();
            let char_before_cursor = if self.state.cursor_col > 0 {
                current_line
                    .chars()
                    .nth(self.state.cursor_col - 1)
                    .map(|ch| ch.to_string())
                    .unwrap_or_default()
            } else {
                String::new()
            };
            if !char_before_cursor.is_empty()
                && char_before_cursor
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
            {
                filtered_text = format!(" {filtered_text}");
            }
        }

        let pasted_lines = split_lines(&filtered_text);
        let total_chars = filtered_text.chars().count();

        if pasted_lines.len() > 10 || total_chars > 1000 {
            let marker = self
                .paste_markers
                .add(&filtered_text, pasted_lines.len() as u64, total_chars as u64);
            self.insert_text_at_cursor_internal(&marker);
            return;
        }

        self.insert_text_at_cursor_internal(&filtered_text);
    }

    fn add_new_line(&mut self) {
        self.cancel_autocomplete();
        self.exit_history_browsing();
        self.last_action = None;

        self.push_undo_snapshot();

        let current_line = self
            .state
            .lines
            .get(self.state.cursor_line)
            .cloned()
            .unwrap_or_default();

        let cursor_col = self.state.cursor_col.min(current_line.len());
        let before = current_line.get(..cursor_col).unwrap_or("").to_string();
        let after = current_line.get(cursor_col..).unwrap_or("").to_string();

        self.state.lines[self.state.cursor_line] = before;
        self.state.lines.insert(self.state.cursor_line + 1, after);

        self.state.cursor_line += 1;
        self.set_cursor_col(0);

        self.notify_change();
    }

    fn should_submit_on_backslash_enter(&self, data: &str) -> bool {
        if self.disable_submit {
            return false;
        }
        if !matches_key(data, "enter") {
            return false;
        }
        let submit_keys = get_keybindings().get_keys("tui.input.submit");
        let has_shift_enter = submit_keys
            .iter()
            .any(|key| key == "shift+enter" || key == "shift+return");
        if !has_shift_enter {
            return false;
        }

        let current_line = self
            .state
            .lines
            .get(self.state.cursor_line)
            .cloned()
            .unwrap_or_default();
        self.state.cursor_col > 0
            && current_line
                .chars()
                .nth(self.state.cursor_col - 1)
                .is_some_and(|ch| ch == '\\')
    }

    fn submit_value(&mut self) {
        self.cancel_autocomplete();
        let result = self
            .paste_markers
            .expand(&self.state.lines.join("\n"))
            .trim()
            .to_string();

        self.state = EditorState::default();
        self.paste_markers.clear();
        self.image_markers.clear();
        self.exit_history_browsing();
        self.scroll_offset = 0;
        self.undo_stack.clear();
        self.last_action = None;

        self.notify_change();
        if let Some(callback) = self.on_submit.as_mut() {
            callback(&result);
        }
    }

    fn handle_backspace(&mut self) {
        self.exit_history_browsing();
        self.last_action = None;

        if self.state.cursor_col > 0 {
            self.push_undo_snapshot();

            let mut line = self
                .state
                .lines
                .get(self.state.cursor_line)
                .cloned()
                .unwrap_or_default();
            let cursor_col = self.state.cursor_col.min(line.len());
            let before_cursor = line.get(..cursor_col).unwrap_or("").to_string();

            let segments = self.segment(&before_cursor, SegmentMode::Grapheme);
            let last_grapheme = segments.last().cloned();
            let grapheme_length = last_grapheme
                .as_ref()
                .map(|segment| segment.segment.len())
                .unwrap_or(1);
            let last_segment_text = last_grapheme
                .as_ref()
                .map(|segment| segment.segment.clone())
                .unwrap_or_default();
            if self.remove_marker_segment(&last_segment_text) {
                self.set_cursor_col(self.state.cursor_col.saturating_sub(grapheme_length));
            } else {
                line = self
                    .state
                    .lines
                    .get(self.state.cursor_line)
                    .cloned()
                    .unwrap_or_default();
                let before = line
                    .get(..self.state.cursor_col.saturating_sub(grapheme_length))
                    .unwrap_or("")
                    .to_string();
                let after = line.get(self.state.cursor_col.min(line.len())..).unwrap_or("").to_string();
                self.state.lines[self.state.cursor_line] = format!("{before}{after}");
                self.set_cursor_col(self.state.cursor_col.saturating_sub(grapheme_length));
            }
        } else if self.state.cursor_line > 0 {
            self.push_undo_snapshot();

            let current_line = self
                .state
                .lines
                .get(self.state.cursor_line)
                .cloned()
                .unwrap_or_default();
            let previous_line = self
                .state
                .lines
                .get(self.state.cursor_line - 1)
                .cloned()
                .unwrap_or_default();

            self.state.lines[self.state.cursor_line - 1] = format!("{previous_line}{current_line}");
            self.state.lines.remove(self.state.cursor_line);

            self.state.cursor_line -= 1;
            self.set_cursor_col(previous_line.len());
        }

        self.notify_change();

        if self.autocomplete_state.is_some() {
            self.update_autocomplete();
        } else {
            let current_line = self
                .state
                .lines
                .get(self.state.cursor_line)
                .cloned()
                .unwrap_or_default();
            let text_before_cursor = current_line
                .get(..self.state.cursor_col.min(current_line.len()))
                .unwrap_or("")
                .to_string();
            if self.is_in_slash_command_context(&text_before_cursor)
                || self.autocomplete_trigger_pattern.is_match(&text_before_cursor)
            {
                self.try_trigger_autocomplete(false);
            }
        }
    }

    /// Set cursor column and clear preferredVisualCol.
    fn set_cursor_col(&mut self, col: usize) {
        self.state.cursor_col = col;
        self.preferred_visual_col = None;
        self.snapped_from_cursor_col = None;
    }

    fn move_to_visual_line(
        &mut self,
        visual_lines: &[VisualLine],
        current_visual_line: usize,
        target_visual_line: usize,
    ) {
        let Some(current_vl) = visual_lines.get(current_visual_line).copied() else {
            return;
        };
        let Some(target_vl) = visual_lines.get(target_visual_line).copied() else {
            return;
        };

        let current_visual_col = match self.snapped_from_cursor_col {
            Some(snapped) => {
                let vl_index = find_visual_line_at(visual_lines, current_vl.logical_line, snapped);
                snapped.saturating_sub(visual_lines[vl_index].start_col)
            }
            None => self.state.cursor_col.saturating_sub(current_vl.start_col),
        };

        let is_last_source_segment = current_visual_line == visual_lines.len().saturating_sub(1)
            || visual_lines
                .get(current_visual_line + 1)
                .map(|next| next.logical_line != current_vl.logical_line)
                .unwrap_or(true);
        let source_max_visual_col = if is_last_source_segment {
            current_vl.length
        } else {
            current_vl.length.saturating_sub(1)
        };

        let is_last_target_segment = target_visual_line == visual_lines.len().saturating_sub(1)
            || visual_lines
                .get(target_visual_line + 1)
                .map(|next| next.logical_line != target_vl.logical_line)
                .unwrap_or(true);
        let target_max_visual_col = if is_last_target_segment {
            target_vl.length
        } else {
            target_vl.length.saturating_sub(1)
        };

        let move_to_visual_col =
            self.compute_vertical_move_column(current_visual_col, source_max_visual_col, target_max_visual_col);

        self.state.cursor_line = target_vl.logical_line;
        let target_col = target_vl.start_col + move_to_visual_col;
        let logical_line = self
            .state
            .lines
            .get(target_vl.logical_line)
            .cloned()
            .unwrap_or_default();
        self.state.cursor_col = target_col.min(logical_line.len());

        let segments = self.segment(&logical_line, SegmentMode::Grapheme);
        for segment in segments {
            if segment.index > self.state.cursor_col {
                break;
            }
            if segment.segment.len() <= 1 {
                continue;
            }
            if self.state.cursor_col < segment.index + segment.segment.len() {
                let is_continuation = segment.index < target_vl.start_col;
                let is_moving_down = target_visual_line > current_visual_line;

                if is_continuation && is_moving_down {
                    let seg_end = segment.index + segment.segment.len();
                    let mut next = target_visual_line + 1;
                    while next < visual_lines.len()
                        && visual_lines[next].logical_line == target_vl.logical_line
                        && visual_lines[next].start_col < seg_end
                    {
                        next += 1;
                    }
                    if next < visual_lines.len() {
                        self.move_to_visual_line(visual_lines, current_visual_line, next);
                        return;
                    }
                }

                self.snapped_from_cursor_col = Some(self.state.cursor_col);
                self.state.cursor_col = segment.index;
                return;
            }
        }

        self.snapped_from_cursor_col = None;
    }

    fn compute_vertical_move_column(
        &mut self,
        current_visual_col: usize,
        source_max_visual_col: usize,
        target_max_visual_col: usize,
    ) -> usize {
        let has_preferred = self.preferred_visual_col.is_some();
        let cursor_in_middle = current_visual_col < source_max_visual_col;
        let target_too_short = target_max_visual_col < current_visual_col;

        if !has_preferred || cursor_in_middle {
            if target_too_short {
                self.preferred_visual_col = Some(current_visual_col);
                return target_max_visual_col;
            }

            self.preferred_visual_col = None;
            return current_visual_col;
        }

        let preferred = self.preferred_visual_col.unwrap_or(0);
        let target_cant_fit_preferred = target_max_visual_col < preferred;
        if target_too_short || target_cant_fit_preferred {
            return target_max_visual_col;
        }

        self.preferred_visual_col = None;
        preferred
    }

    fn move_to_line_start(&mut self) {
        self.last_action = None;
        self.set_cursor_col(0);
    }

    fn move_to_line_end(&mut self) {
        self.last_action = None;
        let current_line = self
            .state
            .lines
            .get(self.state.cursor_line)
            .cloned()
            .unwrap_or_default();
        self.set_cursor_col(current_line.len());
    }

    fn delete_to_start_of_line(&mut self) {
        self.exit_history_browsing();

        let current_line = self
            .state
            .lines
            .get(self.state.cursor_line)
            .cloned()
            .unwrap_or_default();

        if self.state.cursor_col > 0 {
            self.push_undo_snapshot();

            let deleted_text = current_line
                .get(..self.state.cursor_col.min(current_line.len()))
                .unwrap_or("")
                .to_string();
            let accumulate = self.last_action == Some(LastAction::Kill);
            self.kill_ring.push(&deleted_text, true, accumulate);
            self.last_action = Some(LastAction::Kill);

            self.state.lines[self.state.cursor_line] = current_line
                .get(self.state.cursor_col.min(current_line.len())..)
                .unwrap_or("")
                .to_string();
            self.set_cursor_col(0);
        } else if self.state.cursor_line > 0 {
            self.push_undo_snapshot();

            let accumulate = self.last_action == Some(LastAction::Kill);
            self.kill_ring.push("\n", true, accumulate);
            self.last_action = Some(LastAction::Kill);

            let previous_line = self
                .state
                .lines
                .get(self.state.cursor_line - 1)
                .cloned()
                .unwrap_or_default();
            self.state.lines[self.state.cursor_line - 1] = format!("{previous_line}{current_line}");
            self.state.lines.remove(self.state.cursor_line);
            self.state.cursor_line -= 1;
            self.set_cursor_col(previous_line.len());
        }

        self.notify_change();
    }

    fn delete_to_end_of_line(&mut self) {
        self.exit_history_browsing();

        let current_line = self
            .state
            .lines
            .get(self.state.cursor_line)
            .cloned()
            .unwrap_or_default();

        if self.state.cursor_col < current_line.len() {
            self.push_undo_snapshot();

            let deleted_text = current_line
                .get(self.state.cursor_col..)
                .unwrap_or("")
                .to_string();
            let accumulate = self.last_action == Some(LastAction::Kill);
            self.kill_ring.push(&deleted_text, false, accumulate);
            self.last_action = Some(LastAction::Kill);

            self.state.lines[self.state.cursor_line] = current_line
                .get(..self.state.cursor_col)
                .unwrap_or("")
                .to_string();
        } else if self.state.cursor_line < self.state.lines.len().saturating_sub(1) {
            self.push_undo_snapshot();

            let accumulate = self.last_action == Some(LastAction::Kill);
            self.kill_ring.push("\n", false, accumulate);
            self.last_action = Some(LastAction::Kill);

            let next_line = self
                .state
                .lines
                .get(self.state.cursor_line + 1)
                .cloned()
                .unwrap_or_default();
            self.state.lines[self.state.cursor_line] = format!("{current_line}{next_line}");
            self.state.lines.remove(self.state.cursor_line + 1);
        }

        self.notify_change();
    }

    fn delete_word_backwards(&mut self) {
        self.exit_history_browsing();

        let current_line = self
            .state
            .lines
            .get(self.state.cursor_line)
            .cloned()
            .unwrap_or_default();

        if self.state.cursor_col == 0 {
            if self.state.cursor_line > 0 {
                self.push_undo_snapshot();

                let accumulate = self.last_action == Some(LastAction::Kill);
                self.kill_ring.push("\n", true, accumulate);
                self.last_action = Some(LastAction::Kill);

                let previous_line = self
                    .state
                    .lines
                    .get(self.state.cursor_line - 1)
                    .cloned()
                    .unwrap_or_default();
                self.state.lines[self.state.cursor_line - 1] = format!("{previous_line}{current_line}");
                self.state.lines.remove(self.state.cursor_line);
                self.state.cursor_line -= 1;
                self.set_cursor_col(previous_line.len());
            }
        } else {
            self.push_undo_snapshot();

            let was_kill = self.last_action == Some(LastAction::Kill);

            let old_cursor_col = self.state.cursor_col;
            self.move_word_backwards();
            let delete_from = self.state.cursor_col;
            self.set_cursor_col(old_cursor_col);

            let deleted_text = current_line
                .get(delete_from..self.state.cursor_col)
                .unwrap_or("")
                .to_string();
            self.kill_ring.push(&deleted_text, true, was_kill);
            self.last_action = Some(LastAction::Kill);

            let cursor_col = self.state.cursor_col;
            self.state.lines[self.state.cursor_line] = format!(
                "{}{}",
                current_line.get(..delete_from).unwrap_or(""),
                current_line.get(cursor_col..).unwrap_or("")
            );
            self.set_cursor_col(delete_from);
        }

        self.notify_change();
    }

    fn delete_word_forward(&mut self) {
        self.exit_history_browsing();

        let current_line = self
            .state
            .lines
            .get(self.state.cursor_line)
            .cloned()
            .unwrap_or_default();

        if self.state.cursor_col >= current_line.len() {
            if self.state.cursor_line < self.state.lines.len().saturating_sub(1) {
                self.push_undo_snapshot();

                let accumulate = self.last_action == Some(LastAction::Kill);
                self.kill_ring.push("\n", false, accumulate);
                self.last_action = Some(LastAction::Kill);

                let next_line = self
                    .state
                    .lines
                    .get(self.state.cursor_line + 1)
                    .cloned()
                    .unwrap_or_default();
                self.state.lines[self.state.cursor_line] = format!("{current_line}{next_line}");
                self.state.lines.remove(self.state.cursor_line + 1);
            }
        } else {
            self.push_undo_snapshot();

            let was_kill = self.last_action == Some(LastAction::Kill);

            let old_cursor_col = self.state.cursor_col;
            self.move_word_forwards();
            let delete_to = self.state.cursor_col;
            self.set_cursor_col(old_cursor_col);

            let deleted_text = current_line
                .get(self.state.cursor_col..delete_to)
                .unwrap_or("")
                .to_string();
            self.kill_ring.push(&deleted_text, false, was_kill);
            self.last_action = Some(LastAction::Kill);

            let cursor_col = self.state.cursor_col;
            self.state.lines[self.state.cursor_line] = format!(
                "{}{}",
                current_line.get(..cursor_col).unwrap_or(""),
                current_line.get(delete_to..).unwrap_or("")
            );
        }

        self.notify_change();
    }

    fn handle_forward_delete(&mut self) {
        self.exit_history_browsing();
        self.last_action = None;

        let current_line = self
            .state
            .lines
            .get(self.state.cursor_line)
            .cloned()
            .unwrap_or_default();

        if self.state.cursor_col < current_line.len() {
            self.push_undo_snapshot();

            let after_cursor = current_line
                .get(self.state.cursor_col..)
                .unwrap_or("")
                .to_string();

            let segments = self.segment(&after_cursor, SegmentMode::Grapheme);
            let first_segment = segments.first().cloned();
            let first_segment_text = first_segment
                .as_ref()
                .map(|segment| segment.segment.clone())
                .unwrap_or_default();
            if self.remove_marker_segment(&first_segment_text) {
                self.notify_change();
                return;
            }
            let grapheme_length = first_segment
                .as_ref()
                .map(|segment| segment.segment.len())
                .unwrap_or(1);

            let before = current_line
                .get(..self.state.cursor_col)
                .unwrap_or("")
                .to_string();
            let after = current_line
                .get((self.state.cursor_col + grapheme_length).min(current_line.len())..)
                .unwrap_or("")
                .to_string();
            self.state.lines[self.state.cursor_line] = format!("{before}{after}");
        } else if self.state.cursor_line < self.state.lines.len().saturating_sub(1) {
            self.push_undo_snapshot();

            let next_line = self
                .state
                .lines
                .get(self.state.cursor_line + 1)
                .cloned()
                .unwrap_or_default();
            self.state.lines[self.state.cursor_line] = format!("{current_line}{next_line}");
            self.state.lines.remove(self.state.cursor_line + 1);
        }

        self.notify_change();

        if self.autocomplete_state.is_some() {
            self.update_autocomplete();
        } else {
            let current_line = self
                .state
                .lines
                .get(self.state.cursor_line)
                .cloned()
                .unwrap_or_default();
            let text_before_cursor = current_line
                .get(..self.state.cursor_col.min(current_line.len()))
                .unwrap_or("")
                .to_string();
            if self.is_in_slash_command_context(&text_before_cursor)
                || self.autocomplete_trigger_pattern.is_match(&text_before_cursor)
            {
                self.try_trigger_autocomplete(false);
            }
        }
    }

    /// Build a mapping from visual lines to logical positions.
    fn build_visual_line_map(&mut self, width: usize) -> Vec<VisualLine> {
        let mut visual_lines: Vec<VisualLine> = Vec::new();

        for i in 0..self.state.lines.len() {
            let line = self.state.lines.get(i).cloned().unwrap_or_default();
            let line_vis_width = visible_width(&line);
            if line.is_empty() {
                visual_lines.push(VisualLine {
                    logical_line: i,
                    start_col: 0,
                    length: 0,
                });
            } else if line_vis_width <= width {
                visual_lines.push(VisualLine {
                    logical_line: i,
                    start_col: 0,
                    length: line.len(),
                });
            } else {
                let chunks = self.get_wrapped_line(i, width).chunks;
                for chunk in chunks {
                    visual_lines.push(VisualLine {
                        logical_line: i,
                        start_col: chunk.start_index,
                        length: chunk.end_index - chunk.start_index,
                    });
                }
            }
        }

        visual_lines
    }

    fn find_current_visual_line(&self, visual_lines: &[VisualLine]) -> usize {
        find_visual_line_at(visual_lines, self.state.cursor_line, self.state.cursor_col)
    }

    fn move_cursor(&mut self, delta_line: i64, delta_col: i64) {
        self.last_action = None;
        let visual_lines = self.build_visual_line_map(self.last_width);
        let current_visual_line = self.find_current_visual_line(&visual_lines);

        if delta_line != 0 {
            let target_visual_line = current_visual_line as i64 + delta_line;

            if target_visual_line >= 0 && (target_visual_line as usize) < visual_lines.len() {
                self.move_to_visual_line(&visual_lines, current_visual_line, target_visual_line as usize);
            }
        }

        if delta_col != 0 {
            let current_line = self
                .state
                .lines
                .get(self.state.cursor_line)
                .cloned()
                .unwrap_or_default();

            if delta_col > 0 {
                if self.state.cursor_col < current_line.len() {
                    let after_cursor = current_line
                        .get(self.state.cursor_col..)
                        .unwrap_or("")
                        .to_string();
                    let segments = self.segment(&after_cursor, SegmentMode::Grapheme);
                    let first = segments.first();
                    self.set_cursor_col(
                        self.state.cursor_col
                            + first.map(|segment| segment.segment.len()).unwrap_or(1),
                    );
                } else if self.state.cursor_line < self.state.lines.len().saturating_sub(1) {
                    self.state.cursor_line += 1;
                    self.set_cursor_col(0);
                } else if let Some(current_vl) = visual_lines.get(current_visual_line) {
                    self.preferred_visual_col = Some(self.state.cursor_col - current_vl.start_col);
                }
            } else if self.state.cursor_col > 0 {
                let before_cursor = current_line
                    .get(..self.state.cursor_col)
                    .unwrap_or("")
                    .to_string();
                let segments = self.segment(&before_cursor, SegmentMode::Grapheme);
                let last = segments.last();
                self.set_cursor_col(
                    self.state.cursor_col
                        - last.map(|segment| segment.segment.len()).unwrap_or(1),
                );
            } else if self.state.cursor_line > 0 {
                self.state.cursor_line -= 1;
                let prev_line = self
                    .state
                    .lines
                    .get(self.state.cursor_line)
                    .cloned()
                    .unwrap_or_default();
                self.set_cursor_col(prev_line.len());
            }
        }

        if self.autocomplete_state.is_some() {
            self.update_autocomplete();
        }
    }

    /// Scroll by a page (direction: -1 for up, 1 for down).
    fn page_scroll(&mut self, direction: i64) {
        self.last_action = None;
        let terminal_rows = self.tui.terminal_rows();
        let page_size = ((terminal_rows as f64 * 0.3).floor() as usize).max(5);

        let visual_lines = self.build_visual_line_map(self.last_width);
        let current_visual_line = self.find_current_visual_line(&visual_lines);
        let target_visual_line = (current_visual_line as i64 + direction * page_size as i64)
            .clamp(0, visual_lines.len().saturating_sub(1) as i64) as usize;

        self.move_to_visual_line(&visual_lines, current_visual_line, target_visual_line);
    }

    fn move_word_backwards(&mut self) {
        self.last_action = None;
        let current_line = self
            .state
            .lines
            .get(self.state.cursor_line)
            .cloned()
            .unwrap_or_default();

        if self.state.cursor_col == 0 {
            if self.state.cursor_line > 0 {
                self.state.cursor_line -= 1;
                let prev_line = self
                    .state
                    .lines
                    .get(self.state.cursor_line)
                    .cloned()
                    .unwrap_or_default();
                self.set_cursor_col(prev_line.len());
            }
            return;
        }

        let cursor_col = self.state.cursor_col;
        let new_col = {
            let this: &Self = self;
            let segment_fn: &dyn Fn(&str) -> Vec<WordSegment<'_>> =
                &|text: &str| this.segment_word(text);
            let options = WordNavigationOptions {
                segment: Some(segment_fn),
                is_atomic_segment: Some(&is_atomic_marker),
            };
            find_word_backward(&current_line, cursor_col, &options)
        };
        self.set_cursor_col(new_col);
    }

    fn move_word_forwards(&mut self) {
        self.last_action = None;
        let current_line = self
            .state
            .lines
            .get(self.state.cursor_line)
            .cloned()
            .unwrap_or_default();

        if self.state.cursor_col >= current_line.len() {
            if self.state.cursor_line < self.state.lines.len().saturating_sub(1) {
                self.state.cursor_line += 1;
                self.set_cursor_col(0);
            }
            return;
        }

        let cursor_col = self.state.cursor_col;
        let new_col = {
            let this: &Self = self;
            let segment_fn: &dyn Fn(&str) -> Vec<WordSegment<'_>> =
                &|text: &str| this.segment_word(text);
            let options = WordNavigationOptions {
                segment: Some(segment_fn),
                is_atomic_segment: Some(&is_atomic_marker),
            };
            find_word_forward(&current_line, cursor_col, &options)
        };
        self.set_cursor_col(new_col);
    }

    fn yank(&mut self) {
        if self.kill_ring.is_empty() {
            return;
        }

        self.push_undo_snapshot();

        let text = self.kill_ring.peek().unwrap_or("").to_string();
        self.insert_yanked_text(&text);

        self.last_action = Some(LastAction::Yank);
    }

    fn yank_pop(&mut self) {
        if self.last_action != Some(LastAction::Yank) || self.kill_ring.len() <= 1 {
            return;
        }

        self.push_undo_snapshot();

        self.delete_yanked_text();

        self.kill_ring.rotate();

        let text = self.kill_ring.peek().unwrap_or("").to_string();
        self.insert_yanked_text(&text);

        self.last_action = Some(LastAction::Yank);
    }

    fn insert_yanked_text(&mut self, text: &str) {
        self.exit_history_browsing();
        let lines = split_lines(text);

        if lines.len() == 1 {
            let current_line = self
                .state
                .lines
                .get(self.state.cursor_line)
                .cloned()
                .unwrap_or_default();
            let cursor_col = self.state.cursor_col.min(current_line.len());
            let before = current_line.get(..cursor_col).unwrap_or("").to_string();
            let after = current_line.get(cursor_col..).unwrap_or("").to_string();
            self.state.lines[self.state.cursor_line] = format!("{before}{text}{after}");
            self.set_cursor_col(self.state.cursor_col + text.len());
        } else {
            let current_line = self
                .state
                .lines
                .get(self.state.cursor_line)
                .cloned()
                .unwrap_or_default();
            let cursor_col = self.state.cursor_col.min(current_line.len());
            let before = current_line.get(..cursor_col).unwrap_or("").to_string();
            let after = current_line.get(cursor_col..).unwrap_or("").to_string();

            self.state.lines[self.state.cursor_line] = format!("{before}{}", lines[0]);

            for (offset, line) in lines
                .iter()
                .enumerate()
                .take(lines.len().saturating_sub(1))
                .skip(1)
            {
                self.state.lines.insert(self.state.cursor_line + offset, line.clone());
            }

            let last_line_index = self.state.cursor_line + lines.len() - 1;
            self.state
                .lines
                .insert(last_line_index, format!("{}{after}", lines[lines.len() - 1]));

            self.state.cursor_line = last_line_index;
            self.set_cursor_col(lines[lines.len() - 1].len());
        }

        self.notify_change();
    }

    fn delete_yanked_text(&mut self) {
        let Some(yanked_text) = self.kill_ring.peek().map(str::to_string) else {
            return;
        };

        let yank_lines = split_lines(&yanked_text);

        if yank_lines.len() == 1 {
            let current_line = self
                .state
                .lines
                .get(self.state.cursor_line)
                .cloned()
                .unwrap_or_default();
            let delete_len = yanked_text.len();
            let before = current_line
                .get(..self.state.cursor_col.saturating_sub(delete_len))
                .unwrap_or("")
                .to_string();
            let after = current_line
                .get(self.state.cursor_col.min(current_line.len())..)
                .unwrap_or("")
                .to_string();
            self.state.lines[self.state.cursor_line] = format!("{before}{after}");
            self.set_cursor_col(self.state.cursor_col.saturating_sub(delete_len));
        } else {
            let start_line = self.state.cursor_line.saturating_sub(yank_lines.len() - 1);
            let start_col = self
                .state
                .lines
                .get(start_line)
                .map(String::len)
                .unwrap_or(0)
                .saturating_sub(yank_lines[0].len());

            let after_cursor = self
                .state
                .lines
                .get(self.state.cursor_line)
                .cloned()
                .unwrap_or_default()
                .get(self.state.cursor_col.min(
                    self.state
                        .lines
                        .get(self.state.cursor_line)
                        .map(String::len)
                        .unwrap_or(0),
                )..)
                .unwrap_or("")
                .to_string();

            let before_yank = self
                .state
                .lines
                .get(start_line)
                .cloned()
                .unwrap_or_default()
                .get(..start_col)
                .unwrap_or("")
                .to_string();

            self.state.lines.splice(
                start_line..start_line + yank_lines.len(),
                std::iter::once(format!("{before_yank}{after_cursor}")),
            );

            self.state.cursor_line = start_line;
            self.set_cursor_col(start_col);
        }

        self.notify_change();
    }

    fn push_undo_snapshot(&mut self) {
        let attachment_state = self
            .snapshot_attachment_state
            .as_ref()
            .and_then(|callback| callback());
        self.undo_stack.push(&EditorSnapshot {
            state: self.state.clone(),
            paste_state: self.paste_markers.snapshot(),
            image_state: self.image_markers.snapshot(),
            attachment_state,
        });
    }

    fn undo(&mut self) {
        self.exit_history_browsing();
        let Some(snapshot) = self.undo_stack.pop() else {
            return;
        };
        self.state = snapshot.state.clone();
        self.paste_markers.restore(&snapshot.paste_state);
        self.image_markers.restore(&snapshot.image_state);
        if let Some(attachment_state) = &snapshot.attachment_state
            && let Some(callback) = self.restore_attachment_state.as_mut()
        {
            callback(attachment_state);
        }
        self.last_action = None;
        self.preferred_visual_col = None;
        self.notify_change();
        self.notify_image_markers_changed();
    }

    fn jump_to_char(&mut self, character: char, direction: JumpMode) {
        self.last_action = None;
        let is_forward = direction == JumpMode::Forward;
        let lines = self.state.lines.clone();

        let mut line_idx = self.state.cursor_line as i64;
        let end = if is_forward { lines.len() as i64 } else { -1 };
        let step = if is_forward { 1 } else { -1 };

        while line_idx != end {
            let line = lines.get(line_idx as usize).cloned().unwrap_or_default();
            let is_current_line = line_idx as usize == self.state.cursor_line;

            let search_from = if is_current_line {
                if is_forward {
                    Some(self.state.cursor_col + 1)
                } else {
                    Some(self.state.cursor_col.saturating_sub(1))
                }
            } else {
                None
            };

            let idx = if is_forward {
                search_from.map_or_else(
                    || line.find(character),
                    |from| {
                        if from <= line.len() {
                            line[from..].find(character).map(|index| index + from)
                        } else {
                            None
                        }
                    },
                )
            } else {
                search_from.map_or_else(
                    || line.rfind(character),
                    |from| {
                        let limit = from.min(line.len());
                        line[..limit].rfind(character)
                    },
                )
            };

            if let Some(index) = idx {
                self.state.cursor_line = line_idx as usize;
                self.set_cursor_col(index);
                return;
            }
            line_idx += step;
        }
    }

    fn is_slash_menu_allowed(&self) -> bool {
        self.state.cursor_line == 0
    }

    fn is_at_start_of_message(&self) -> bool {
        if !self.is_slash_menu_allowed() {
            return false;
        }
        let current_line = self
            .state
            .lines
            .get(self.state.cursor_line)
            .cloned()
            .unwrap_or_default();
        let before_cursor = current_line
            .get(..self.state.cursor_col.min(current_line.len()))
            .unwrap_or("")
            .to_string();
        let trimmed = before_cursor.trim();
        trimmed.is_empty() || trimmed == "/"
    }

    fn is_in_slash_command_context(&self, text_before_cursor: &str) -> bool {
        self.is_slash_menu_allowed() && text_before_cursor.trim_start().starts_with('/')
    }

    fn get_best_autocomplete_match_index(items: &[AutocompleteItem], prefix: &str) -> Option<usize> {
        if prefix.is_empty() {
            return None;
        }

        let mut first_prefix_index: Option<usize> = None;

        for (i, item) in items.iter().enumerate() {
            if item.value == prefix {
                return Some(i);
            }
            if first_prefix_index.is_none() && item.value.starts_with(prefix) {
                first_prefix_index = Some(i);
            }
        }

        first_prefix_index
    }

    fn create_autocomplete_list(&mut self, prefix: &str, items: &[AutocompleteItem]) -> SelectList {
        let layout = if prefix.starts_with('/') {
            SelectListLayoutOptions {
                min_primary_column_width: Some(SLASH_COMMAND_MIN_PRIMARY_COLUMN_WIDTH),
                max_primary_column_width: Some(SLASH_COMMAND_MAX_PRIMARY_COLUMN_WIDTH),
                truncate_primary: None,
            }
        } else {
            SelectListLayoutOptions::default()
        };
        let select_items: Vec<SelectItem> = items
            .iter()
            .map(|item| SelectItem {
                value: item.value.clone(),
                label: item.label.clone(),
                description: item.description.clone(),
            })
            .collect();
        let theme = SelectListTheme {
            selected_prefix: self.theme.select_list.selected_prefix.clone(),
            selected_text: self.theme.select_list.selected_text.clone(),
            description: self.theme.select_list.description.clone(),
            scroll_info: self.theme.select_list.scroll_info.clone(),
            no_match: self.theme.select_list.no_match.clone(),
            render_row: self.theme.select_list.render_row.clone(),
        };
        let mut list = SelectList::new(select_items, self.autocomplete_max_visible, theme, layout);
        let pending = self.pending_autocomplete_select.clone();
        list.on_select = Some(Box::new(move |selected: &SelectItem| {
            *pending.borrow_mut() = Some(selected.clone());
        }));
        list
    }

    fn drain_pending_autocomplete_select(&mut self) {
        let selected = self.pending_autocomplete_select.borrow_mut().take();
        let Some(selected) = selected else {
            return;
        };
        let Some(provider) = self.autocomplete_provider.clone() else {
            return;
        };
        self.push_undo_snapshot();
        self.last_action = None;
        let item = AutocompleteItem {
            value: selected.value.clone(),
            label: selected.label.clone(),
            description: selected.description.clone(),
        };
        let result = provider.borrow().apply_completion(
            &self.state.lines,
            self.state.cursor_line,
            self.state.cursor_col,
            &item,
            &self.autocomplete_prefix,
        );
        self.state.lines = result.lines;
        self.state.cursor_line = result.cursor_line;
        self.set_cursor_col(result.cursor_col);
        self.cancel_autocomplete();
        self.notify_change();
    }

    fn try_trigger_autocomplete(&mut self, explicit_tab: bool) {
        self.request_autocomplete(false, explicit_tab);
    }

    fn handle_tab_completion(&mut self) {
        if self.autocomplete_provider.is_none() {
            return;
        }

        let current_line = self
            .state
            .lines
            .get(self.state.cursor_line)
            .cloned()
            .unwrap_or_default();
        let before_cursor = current_line
            .get(..self.state.cursor_col.min(current_line.len()))
            .unwrap_or("")
            .to_string();

        if self.is_in_slash_command_context(&before_cursor)
            && !before_cursor.trim_start().contains(' ')
        {
            self.handle_slash_command_completion();
        } else {
            self.force_file_autocomplete(true);
        }
    }

    fn handle_slash_command_completion(&mut self) {
        self.request_autocomplete(false, true);
    }

    fn force_file_autocomplete(&mut self, explicit_tab: bool) {
        self.request_autocomplete(true, explicit_tab);
    }

    fn request_autocomplete(&mut self, force: bool, explicit_tab: bool) {
        let Some(provider) = self.autocomplete_provider.clone() else {
            return;
        };

        if force {
            let should_trigger = provider.borrow().should_trigger_file_completion(
                &self.state.lines,
                self.state.cursor_line,
                self.state.cursor_col,
            );
            if !should_trigger {
                return;
            }
        }

        self.cancel_autocomplete_request();

        self.run_autocomplete_request(force, explicit_tab);
    }

    fn set_autocomplete_trigger_characters(&mut self, trigger_characters: &[String]) {
        let mut next: Vec<String> = DEFAULT_AUTOCOMPLETE_TRIGGER_CHARACTERS
            .iter()
            .map(|character| (*character).to_string())
            .collect();
        for character in trigger_characters {
            if character.chars().count() != 1
                || character == "/"
                || is_whitespace_char(character)
                || next.contains(character)
            {
                continue;
            }
            next.push(character.clone());
        }
        self.autocomplete_trigger_characters = next.clone();
        self.autocomplete_trigger_pattern = build_trigger_pattern(&next);
        self.autocomplete_debounce_pattern = build_debounce_pattern(&next);
    }

    fn run_autocomplete_request(&mut self, force: bool, explicit_tab: bool) {
        let Some(provider) = self.autocomplete_provider.clone() else {
            return;
        };

        let snapshot_text = self.get_text();
        let snapshot_line = self.state.cursor_line;
        let snapshot_col = self.state.cursor_col;

        let suggestions = provider.borrow_mut().get_suggestions(
            &self.state.lines,
            self.state.cursor_line,
            self.state.cursor_col,
            force,
        );

        if !self.is_autocomplete_request_current(&snapshot_text, snapshot_line, snapshot_col) {
            return;
        }

        let Some(suggestions) = suggestions else {
            self.cancel_autocomplete();
            self.tui.request_render();
            return;
        };
        if suggestions.items.is_empty() {
            self.cancel_autocomplete();
            self.tui.request_render();
            return;
        }

        if force && explicit_tab && suggestions.items.len() == 1 {
            let item = suggestions.items[0].clone();
            self.push_undo_snapshot();
            self.last_action = None;
            let result = provider.borrow().apply_completion(
                &self.state.lines,
                self.state.cursor_line,
                self.state.cursor_col,
                &item,
                &suggestions.prefix,
            );
            self.state.lines = result.lines;
            self.state.cursor_line = result.cursor_line;
            self.set_cursor_col(result.cursor_col);
            self.notify_change();
            self.tui.request_render();
            return;
        }

        let state = if force {
            AutocompleteState::Force
        } else {
            AutocompleteState::Regular
        };
        self.apply_autocomplete_suggestions(&suggestions, state);
        self.tui.request_render();
    }

    fn is_autocomplete_request_current(
        &self,
        snapshot_text: &str,
        snapshot_line: usize,
        snapshot_col: usize,
    ) -> bool {
        self.get_text() == snapshot_text
            && self.state.cursor_line == snapshot_line
            && self.state.cursor_col == snapshot_col
    }

    fn apply_autocomplete_suggestions(
        &mut self,
        suggestions: &crate::autocomplete::AutocompleteSuggestions,
        state: AutocompleteState,
    ) {
        self.autocomplete_prefix = suggestions.prefix.clone();
        self.autocomplete_list = Some(self.create_autocomplete_list(&suggestions.prefix, &suggestions.items));

        if let Some(best_match_index) =
            Self::get_best_autocomplete_match_index(&suggestions.items, &suggestions.prefix)
            && let Some(list) = self.autocomplete_list.as_mut()
        {
            list.set_selected_index(best_match_index);
        }

        self.autocomplete_state = Some(state);
    }

    fn cancel_autocomplete_request(&mut self) {
        *self.pending_autocomplete_select.borrow_mut() = None;
    }

    fn clear_autocomplete_ui(&mut self) {
        self.autocomplete_state = None;
        self.autocomplete_list = None;
        self.autocomplete_prefix = String::new();
    }

    pub fn cancel_autocomplete(&mut self) {
        self.cancel_autocomplete_request();
        self.clear_autocomplete_ui();
    }

    pub fn is_showing_autocomplete(&self) -> bool {
        self.autocomplete_state.is_some()
    }

    fn update_autocomplete(&mut self) {
        if self.autocomplete_state.is_none() || self.autocomplete_provider.is_none() {
            return;
        }
        let force = self.autocomplete_state == Some(AutocompleteState::Force);
        self.request_autocomplete(force, false);
    }

    pub fn handle_editor_input(&mut self, data: &str) {
        let data = normalize_warp_wsl_shift_enter_input(
            data,
            &self.terminal_environment,
            self.terminal_platform.as_deref().unwrap_or("linux"),
            self.terminal_socket_exists.as_deref(),
        );
        let kb = get_keybindings();

        if self.jump_mode.is_some() {
            if kb.matches(&data, "tui.editor.jumpForward") || kb.matches(&data, "tui.editor.jumpBackward") {
                self.jump_mode = None;
                return;
            }

            let printable = decode_printable_key(&data).or_else(|| {
                if data.chars().next().is_some_and(|ch| (ch as u32) >= 32) {
                    Some(data.clone())
                } else {
                    None
                }
            });
            if let Some(printable) = printable {
                let direction = self.jump_mode.expect("jump mode set");
                self.jump_mode = None;
                if let Some(character) = printable.chars().next() {
                    self.jump_to_char(character, direction);
                }
                return;
            }

            self.jump_mode = None;
        }

        let mut data = data;

        if data.contains("\x1b[200~") {
            self.is_in_paste = true;
            self.paste_buffer = String::new();
            data = data.replace("\x1b[200~", "");
        }

        if self.is_in_paste {
            self.paste_buffer.push_str(&data);
            if let Some(end_index) = self.paste_buffer.find("\x1b[201~") {
                let paste_content = self.paste_buffer[..end_index].to_string();
                if !paste_content.is_empty() {
                    self.handle_paste(&paste_content);
                }
                self.is_in_paste = false;
                let remaining = self.paste_buffer[end_index + 6..].to_string();
                self.paste_buffer = String::new();
                if !remaining.is_empty() {
                    self.handle_editor_input(&remaining);
                }
                return;
            }
            return;
        }

        if kb.matches(&data, "tui.input.copy") {
            return;
        }

        if kb.matches(&data, "tui.editor.undo") {
            self.undo();
            return;
        }

        if self.autocomplete_state.is_some() && self.autocomplete_list.is_some() {
            if kb.matches(&data, "tui.select.cancel") {
                self.cancel_autocomplete();
                return;
            }

            if kb.matches(&data, "tui.select.up") || kb.matches(&data, "tui.select.down") {
                if let Some(list) = self.autocomplete_list.as_mut() {
                    list.handle_input(&data);
                }
                return;
            }

            if kb.matches(&data, "tui.input.tab") {
                let selected = self
                    .autocomplete_list
                    .as_ref()
                    .and_then(|list| list.get_selected_item().cloned());
                if let (Some(selected), Some(provider)) = (selected, self.autocomplete_provider.clone()) {
                    self.push_undo_snapshot();
                    self.last_action = None;
                    let item = AutocompleteItem {
                        value: selected.value.clone(),
                        label: selected.label.clone(),
                        description: selected.description.clone(),
                    };
                    let result = provider.borrow().apply_completion(
                        &self.state.lines,
                        self.state.cursor_line,
                        self.state.cursor_col,
                        &item,
                        &self.autocomplete_prefix,
                    );
                    self.state.lines = result.lines;
                    self.state.cursor_line = result.cursor_line;
                    self.set_cursor_col(result.cursor_col);
                    self.cancel_autocomplete();
                    self.notify_change();
                }
                return;
            }

            if kb.matches(&data, "tui.select.confirm") {
                let selected = self
                    .autocomplete_list
                    .as_ref()
                    .and_then(|list| list.get_selected_item().cloned());
                if let (Some(selected), Some(provider)) = (selected, self.autocomplete_provider.clone()) {
                    self.push_undo_snapshot();
                    self.last_action = None;
                    let item = AutocompleteItem {
                        value: selected.value.clone(),
                        label: selected.label.clone(),
                        description: selected.description.clone(),
                    };
                    let result = provider.borrow().apply_completion(
                        &self.state.lines,
                        self.state.cursor_line,
                        self.state.cursor_col,
                        &item,
                        &self.autocomplete_prefix,
                    );
                    self.state.lines = result.lines;
                    self.state.cursor_line = result.cursor_line;
                    self.set_cursor_col(result.cursor_col);

                    if self.autocomplete_prefix.starts_with('/') {
                        self.cancel_autocomplete();
                    } else {
                        self.cancel_autocomplete();
                        self.notify_change();
                        return;
                    }
                }
            }
        }

        if kb.matches(&data, "tui.input.tab") && self.autocomplete_state.is_none() {
            self.handle_tab_completion();
            return;
        }

        if kb.matches(&data, "tui.editor.deleteToLineEnd") {
            self.delete_to_end_of_line();
            return;
        }
        if kb.matches(&data, "tui.editor.deleteToLineStart") {
            self.delete_to_start_of_line();
            return;
        }
        if kb.matches(&data, "tui.editor.deleteWordBackward") {
            self.delete_word_backwards();
            return;
        }
        if kb.matches(&data, "tui.editor.deleteWordForward") {
            self.delete_word_forward();
            return;
        }
        if kb.matches(&data, "tui.editor.deleteCharBackward") || matches_key(&data, "shift+backspace") {
            self.handle_backspace();
            return;
        }
        if kb.matches(&data, "tui.editor.deleteCharForward") || matches_key(&data, "shift+delete") {
            self.handle_forward_delete();
            return;
        }

        if kb.matches(&data, "tui.editor.yank") {
            self.yank();
            return;
        }
        if kb.matches(&data, "tui.editor.yankPop") {
            self.yank_pop();
            return;
        }

        if kb.matches(&data, "tui.editor.historyPrevious") {
            self.cancel_autocomplete();
            self.navigate_history(-1);
            return;
        }
        if kb.matches(&data, "tui.editor.historyNext") {
            self.cancel_autocomplete();
            self.navigate_history(1);
            return;
        }

        if kb.matches(&data, "tui.editor.cursorLineStart") {
            self.move_to_line_start();
            return;
        }
        if kb.matches(&data, "tui.editor.cursorLineEnd") {
            self.move_to_line_end();
            return;
        }
        if kb.matches(&data, "tui.editor.cursorWordLeft") {
            self.move_word_backwards();
            return;
        }
        if kb.matches(&data, "tui.editor.cursorWordRight") {
            self.move_word_forwards();
            return;
        }

        if kb.matches(&data, "tui.input.submit") {
            if self.disable_submit {
                return;
            }

            let current_line = self
                .state
                .lines
                .get(self.state.cursor_line)
                .cloned()
                .unwrap_or_default();
            if self.state.cursor_col > 0
                && current_line
                    .chars()
                    .nth(self.state.cursor_col - 1)
                    .is_some_and(|ch| ch == '\\')
            {
                self.handle_backspace();
                self.add_new_line();
                return;
            }

            self.submit_value();
            return;
        }

        let first_char_code = data.chars().next().map(|ch| ch as u32).unwrap_or(0);
        if kb.matches(&data, "tui.input.newLine")
            || (first_char_code == 10 && data.chars().count() > 1)
            || data == "\x1b\r"
            || data == "\x1b[13;2~"
            || (data.chars().count() > 1 && data.contains('\x1b') && data.contains('\r'))
            || data == "\n"
        {
            if self.should_submit_on_backslash_enter(&data) {
                self.handle_backspace();
                self.submit_value();
                return;
            }
            self.add_new_line();
            return;
        }

        if kb.matches(&data, "tui.editor.cursorUp") {
            if self.is_on_first_visual_line()
                && (self.is_editor_empty() || self.history_index > -1 || self.state.cursor_col == 0)
            {
                self.navigate_history(-1);
            } else if self.is_on_first_visual_line() {
                self.move_to_line_start();
            } else {
                self.move_cursor(-1, 0);
            }
            return;
        }
        if kb.matches(&data, "tui.editor.cursorDown") {
            if self.history_index > -1 && self.is_on_last_visual_line() {
                self.navigate_history(1);
            } else if self.is_on_last_visual_line() {
                self.move_to_line_end();
            } else {
                self.move_cursor(1, 0);
            }
            return;
        }
        if kb.matches(&data, "tui.editor.cursorRight") {
            self.move_cursor(0, 1);
            return;
        }
        if kb.matches(&data, "tui.editor.cursorLeft") {
            self.move_cursor(0, -1);
            return;
        }

        if kb.matches(&data, "tui.editor.pageUp") {
            self.page_scroll(-1);
            return;
        }
        if kb.matches(&data, "tui.editor.pageDown") {
            self.page_scroll(1);
            return;
        }

        if kb.matches(&data, "tui.editor.jumpForward") {
            self.jump_mode = Some(JumpMode::Forward);
            return;
        }
        if kb.matches(&data, "tui.editor.jumpBackward") {
            self.jump_mode = Some(JumpMode::Backward);
            return;
        }

        if matches_key(&data, "shift+space") {
            self.insert_character(" ", false);
            return;
        }

        if let Some(printable) = decode_printable_key(&data) {
            self.insert_character(&printable, false);
            return;
        }

        if first_char_code >= 32 {
            self.insert_character(&data, false);
        }
    }
}

impl Component for Editor {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.render_editor(width)
    }

    fn handle_input(&mut self, data: &str) {
        self.handle_editor_input(data);
    }

    fn has_input_handler(&self) -> bool {
        true
    }

    fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.handle_editor_mouse(event)
    }

    fn invalidate(&mut self) {
        self.wrapped_line_cache.clear();
    }

    fn focusable_get(&self) -> Option<bool> {
        Some(self.focused)
    }

    fn focusable_set(&mut self, focused: bool) {
        self.focused = focused;
    }
}

impl Focusable for Editor {
    fn focused(&self) -> bool {
        self.focused
    }

    fn set_focused(&mut self, value: bool) {
        self.focused = value;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CursorPlacement {
    Start,
    End,
}

/// JS `String.prototype.split("\n")`.
fn split_lines(text: &str) -> Vec<String> {
    text.split('\n').map(str::to_string).collect()
}

/// Normalize text for editor storage: line endings to `\n`, tabs to four spaces.
pub fn normalize_text(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n").replace('\t', "    ")
}

/// Some terminals re-encode control bytes inside bracketed paste as CSI-u Ctrl+<letter>
/// sequences (`ESC [ <codepoint> ; 5 u`); decode those back to their literal byte.
fn decode_bracketed_paste_ctrl_sequences(text: &str) -> String {
    static PATTERN: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"\x1b\[(\d+);5u").expect("valid bracketed paste ctrl sequence regex")
    });
    PATTERN
        .replace_all(text, |captures: &regex::Captures<'_>| {
            let code: u32 = captures
                .get(1)
                .and_then(|m| m.as_str().parse().ok())
                .unwrap_or(0);
            if (97..=122).contains(&code) {
                char::from_u32(code - 96).map(String::from).unwrap_or_else(|| {
                    captures.get(0).expect("group 0").as_str().to_string()
                })
            } else if (65..=90).contains(&code) {
                char::from_u32(code - 64).map(String::from).unwrap_or_else(|| {
                    captures.get(0).expect("group 0").as_str().to_string()
                })
            } else {
                captures.get(0).expect("group 0").as_str().to_string()
            }
        })
        .into_owned()
}

fn find_visual_line_at(visual_lines: &[VisualLine], line: usize, col: usize) -> usize {
    for (i, vl) in visual_lines.iter().enumerate() {
        if vl.logical_line != line {
            continue;
        }
        let offset = col as i64 - vl.start_col as i64;
        let is_last_segment_of_line = i == visual_lines.len().saturating_sub(1)
            || visual_lines
                .get(i + 1)
                .map(|next| next.logical_line != vl.logical_line)
                .unwrap_or(true);
        if offset >= 0 && (offset < vl.length as i64 || (is_last_segment_of_line && offset == vl.length as i64)) {
            return i;
        }
    }
    visual_lines.len().saturating_sub(1)
}

/// Grapheme-atomic segments used by word navigation and grapheme movement.
pub fn word_segments_for_editor(text: &str) -> Vec<MarkerSegment> {
    word_segment_refs(text)
        .into_iter()
        .map(|segment| MarkerSegment {
            index: segment.index,
            segment: segment.segment.to_string(),
        })
        .collect()
}

/// Grapheme segments of `text`, exported for consumers that need marker awareness.
pub fn grapheme_segments_for_editor(text: &str) -> Vec<MarkerSegment> {
    grapheme_segment_refs(text)
        .into_iter()
        .map(|segment| MarkerSegment {
            index: segment.index,
            segment: segment.segment.to_string(),
        })
        .collect()
}

/// Unused import guard for `graphemes` (kept for parity with senpi's segmenter use).
pub fn first_grapheme(text: &str) -> &str {
    graphemes(text).next().unwrap_or("")
}

/// Word segments with `[paste #N]` / `[Image #N]` markers kept atomic, matching the editor's
/// marker-aware `segment(text, "word")`.
pub fn word_segments_with_markers<'a>(text: &'a str, authorized: &BTreeSet<String>) -> Vec<WordSegment<'a>> {
    let base = word_segments(text);
    if authorized.is_empty() {
        return base;
    }
    let refs: Vec<SegmentRef<'_>> = base
        .iter()
        .map(|segment| SegmentRef {
            index: segment.index,
            segment: segment.segment,
        })
        .collect();
    let merged = segment_with_markers(text, &refs, authorized);
    let mut out: Vec<WordSegment<'a>> = Vec::with_capacity(merged.len());
    let mut base_index = 0usize;
    for segment in merged {
        while base_index < base.len() && base[base_index].index < segment.index {
            base_index += 1;
        }
        let is_word_like = match base.get(base_index) {
            Some(candidate)
                if candidate.index == segment.index
                    && candidate.segment.len() == segment.segment.len() =>
            {
                candidate.is_word_like
            }
            _ => true,
        };
        out.push(WordSegment {
            segment: &text[segment.index..segment.index + segment.segment.len()],
            index: segment.index,
            is_word_like,
        });
    }
    out
}
