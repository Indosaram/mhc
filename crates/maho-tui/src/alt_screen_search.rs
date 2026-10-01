//! Port of senpi `packages/tui/src/alt-screen-search.ts`.

use crate::components::input::{Input, InputOptions};
use crate::keybindings::get_keybindings;
use crate::tui::{Component, Focusable};
use crate::utils::{graphemes, is_whitespace_char, strip_terminal_sequences, truncate_to_width, visible_width};

#[derive(Debug, Clone, Copy)]
struct SearchSourceSpan {
    text_start: usize,
    text_end: usize,
    row: usize,
    start_col: usize,
    end_col: usize,
    linear_columns: bool,
}

struct SearchCorpus {
    text: String,
    spans: Vec<SearchSourceSpan>,
}

#[derive(Debug, Clone, Copy)]
pub struct AltScreenSearchSegment {
    pub row: usize,
    pub start_col: usize,
    pub end_col: usize,
}

#[derive(Debug, Clone)]
pub struct AltScreenSearchMatch {
    pub segments: Vec<AltScreenSearchSegment>,
}

fn is_printable_ascii(line: &str) -> bool {
    line.bytes().all(|b| (0x20..=0x7e).contains(&b))
}

fn build_search_corpus(lines: &[String]) -> SearchCorpus {
    let mut text = String::new();
    let mut spans = Vec::new();
    let mut pending_separator = false;

    for (row, raw_line) in lines.iter().enumerate() {
        let line = strip_terminal_sequences(raw_line);
        let mut column = 0usize;

        if is_printable_ascii(&line) {
            let bytes = line.as_bytes();
            let mut index = 0usize;
            while index < bytes.len() {
                if bytes[index] == b' ' {
                    if !text.is_empty() {
                        pending_separator = true;
                    }
                    column += 1;
                    index += 1;
                    continue;
                }
                let mut end = index + 1;
                while end < bytes.len() && bytes[end] != b' ' {
                    end += 1;
                }
                if pending_separator {
                    text.push(' ');
                    pending_separator = false;
                }
                let token = &line[index..end];
                spans.push(SearchSourceSpan {
                    text_start: text.len(),
                    text_end: text.len() + token.len(),
                    row,
                    start_col: column,
                    end_col: column + token.len(),
                    linear_columns: true,
                });
                text.push_str(token);
                column += token.len();
                index = end;
            }
        } else {
            for grapheme in graphemes(&line) {
                let width = visible_width(grapheme);
                if is_whitespace_char(grapheme) {
                    if !text.is_empty() {
                        pending_separator = true;
                    }
                    column += width;
                    continue;
                }
                if pending_separator {
                    text.push(' ');
                    pending_separator = false;
                }
                spans.push(SearchSourceSpan {
                    text_start: text.len(),
                    text_end: text.len() + grapheme.len(),
                    row,
                    start_col: column,
                    end_col: column + width,
                    linear_columns: false,
                });
                text.push_str(grapheme);
                column += width;
            }
        }
        if !text.is_empty() {
            pending_separator = true;
        }
    }

    SearchCorpus { text, spans }
}

fn normalize_query(query: &str) -> String {
    let collapsed: String = query.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed.trim().to_string()
}

fn find_search_corpus_matches(corpus: &SearchCorpus, normalized_query: &str) -> Vec<AltScreenSearchMatch> {
    if normalized_query.is_empty() {
        return Vec::new();
    }
    let haystack_lower = corpus.text.to_lowercase();
    let needle_lower = normalized_query.to_lowercase();
    if needle_lower.is_empty() {
        return Vec::new();
    }

    let mut matches = Vec::new();
    let mut span_index = 0usize;
    let mut search_from = 0usize;

    while let Some(rel_start) = haystack_lower[search_from..].find(&needle_lower) {
        let start = search_from + rel_start;
        let end = start + needle_lower.len();

        while span_index < corpus.spans.len() && corpus.spans[span_index].text_end <= start {
            span_index += 1;
        }

        let mut segments: Vec<AltScreenSearchSegment> = Vec::new();
        for span in &corpus.spans[span_index..] {
            if span.text_start >= end {
                break;
            }
            if span.text_end <= start {
                continue;
            }
            let start_col = if span.linear_columns {
                span.start_col + start.max(span.text_start) - span.text_start
            } else {
                span.start_col
            };
            let end_col = if span.linear_columns {
                span.start_col + end.min(span.text_end) - span.text_start
            } else {
                span.end_col
            };
            if let Some(previous) = segments.last_mut()
                && previous.row == span.row
                && start_col <= previous.end_col
            {
                previous.end_col = previous.end_col.max(end_col);
                continue;
            }
            segments.push(AltScreenSearchSegment { row: span.row, start_col, end_col });
        }
        while span_index < corpus.spans.len() && corpus.spans[span_index].text_end <= end {
            span_index += 1;
        }
        if !segments.is_empty() {
            matches.push(AltScreenSearchMatch { segments });
        }
        search_from = if end > start { end } else { start + 1 };
        if search_from > haystack_lower.len() {
            break;
        }
    }

    matches
}

pub struct AltScreenSearchResult {
    pub matches: Vec<AltScreenSearchMatch>,
    pub changed: bool,
}

/// Caches the searchable corpus and matches while rendered transcript lines remain unchanged.
pub struct AltScreenSearchIndex {
    source_lines: Option<Vec<String>>,
    corpus: Option<SearchCorpus>,
    normalized_query: Option<String>,
    matches: Vec<AltScreenSearchMatch>,
}

impl AltScreenSearchIndex {
    pub fn new() -> Self {
        Self { source_lines: None, corpus: None, normalized_query: None, matches: Vec::new() }
    }

    pub fn search(&mut self, lines: &[String], query: &str) -> AltScreenSearchResult {
        let mut source_changed = self.source_lines.as_ref().map(Vec::len) != Some(lines.len());
        if !source_changed
            && let Some(source_lines) = &self.source_lines
        {
            source_changed = source_lines.iter().zip(lines.iter()).any(|(a, b)| a != b);
        }
        if source_changed || self.corpus.is_none() {
            self.source_lines = Some(lines.to_vec());
            self.corpus = Some(build_search_corpus(lines));
        }

        let normalized_query = normalize_query(query);
        let changed = source_changed || Some(&normalized_query) != self.normalized_query.as_ref();
        let Some(corpus) = self.corpus.as_ref() else {
            return AltScreenSearchResult { matches: self.matches.clone(), changed };
        };
        if changed {
            self.matches = find_search_corpus_matches(corpus, &normalized_query);
            self.normalized_query = Some(normalized_query);
        }
        AltScreenSearchResult { matches: self.matches.clone(), changed }
    }
}

impl Default for AltScreenSearchIndex {
    fn default() -> Self {
        Self::new()
    }
}

pub fn find_alt_screen_search_matches(lines: &[String], query: &str) -> Vec<AltScreenSearchMatch> {
    let normalized_query = normalize_query(query);
    if normalized_query.is_empty() {
        Vec::new()
    } else {
        find_search_corpus_matches(&build_search_corpus(lines), &normalized_query)
    }
}

pub fn get_alt_screen_search_match_key(m: &AltScreenSearchMatch) -> String {
    match (m.segments.first(), m.segments.last()) {
        (Some(first), Some(last)) => format!("{}:{}:{}:{}", first.row, first.start_col, last.row, last.end_col),
        _ => String::new(),
    }
}

pub type NavigationButtonStyle = std::rc::Rc<dyn Fn(&str, bool) -> String>;

pub struct AltScreenSearchComponent {
    input: Input,
    on_query_change: Box<dyn FnMut(&str)>,
    navigation_button_style: NavigationButtonStyle,
    result_count: usize,
    result_index: i64,
    previous_button_start: i64,
    previous_button_end: i64,
    next_button_start: i64,
    next_button_end: i64,
    hovered_navigation_direction: Option<i64>,
    focused: bool,
    is_darwin: bool,
}

impl AltScreenSearchComponent {
    pub fn new(on_query_change: Box<dyn FnMut(&str)>, navigation_button_style: Option<NavigationButtonStyle>, is_darwin: bool) -> Self {
        let mut input = Input::new(InputOptions {
            prompt: Some(" ".to_string()),
            placeholder: Some("Find in transcript".to_string()),
            placeholder_style: Some(std::rc::Rc::new(|text: &str| format!("\x1b[2m{text}\x1b[22m"))),
        });
        input.set_focused(false);
        Self {
            input,
            on_query_change,
            navigation_button_style: navigation_button_style.unwrap_or_else(|| std::rc::Rc::new(|text: &str, _hovered| text.to_string())),
            result_count: 0,
            result_index: -1,
            previous_button_start: -1,
            previous_button_end: -1,
            next_button_start: -1,
            next_button_end: -1,
            hovered_navigation_direction: None,
            focused: false,
            is_darwin,
        }
    }

    pub fn set_result(&mut self, index: i64, count: usize) {
        self.result_index = index;
        self.result_count = count;
    }

    pub fn get_navigation_direction_at(&self, row: i64, column: i64) -> Option<i64> {
        if row != 2 {
            return None;
        }
        if column >= self.previous_button_start && column < self.previous_button_end {
            return Some(-1);
        }
        if column >= self.next_button_start && column < self.next_button_end {
            return Some(1);
        }
        None
    }

    pub fn set_hovered_navigation_direction(&mut self, direction: Option<i64>) -> bool {
        if direction == self.hovered_navigation_direction {
            return false;
        }
        self.hovered_navigation_direction = direction;
        true
    }

    fn format_key(&self, key: Option<&str>) -> String {
        let Some(key) = key else {
            return "Unbound".to_string();
        };
        key.split('+')
            .map(|part| {
                if self.is_darwin && part.eq_ignore_ascii_case("alt") {
                    "Option".to_string()
                } else {
                    let mut chars = part.chars();
                    match chars.next() {
                        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                        None => String::new(),
                    }
                }
            })
            .collect::<Vec<_>>()
            .join("+")
    }
}

impl Focusable for AltScreenSearchComponent {
    fn focused(&self) -> bool {
        self.focused
    }

    fn set_focused(&mut self, value: bool) {
        self.focused = value;
        self.input.set_focused(value);
    }
}

impl Component for AltScreenSearchComponent {
    fn handle_input(&mut self, data: &str) {
        let previous = self.input.get_value().to_string();
        self.input.handle_input(data);
        let query = self.input.get_value().to_string();
        if query != previous {
            (self.on_query_change)(&query);
        }
    }

    fn has_input_handler(&self) -> bool {
        true
    }

    fn invalidate(&mut self) {
        self.input.invalidate();
    }

    fn focusable_get(&self) -> Option<bool> {
        Some(self.focused)
    }

    fn focusable_set(&mut self, focused: bool) {
        self.set_focused(focused);
    }

    fn render(&mut self, width: usize) -> Vec<String> {
        let safe_width = width.max(1);
        let inner_width = safe_width.saturating_sub(2);

        let kb = get_keybindings();
        let previous_key_id = kb.get_keys("tui.altScreen.searchPrevious").into_iter().next();
        let next_key_id = kb.get_keys("tui.altScreen.searchNext").into_iter().next();
        let previous_key = self.format_key(previous_key_id.as_deref());
        let next_key = self.format_key(next_key_id.as_deref());

        let query = self.input.get_value().to_string();
        let result_text_body = if query.is_empty() {
            String::new()
        } else if self.result_count == 0 {
            "No matches".to_string()
        } else {
            format!("{}/{}", self.result_index + 1, self.result_count)
        };
        let result_space = inner_width.saturating_sub(3);
        let visible_result = truncate_to_width(&result_text_body, result_space, "", false);
        let result_text = if visible_result.is_empty() { String::new() } else { format!("\x1b[2m {visible_result} \x1b[22m") };
        let input_width = inner_width.saturating_sub(visible_width(&result_text));
        let rendered_input_line = self.input.render(input_width.max(1));
        let input_line = truncate_to_width(rendered_input_line.first().map(String::as_str).unwrap_or(""), input_width, "", false);
        let input_padding = " ".repeat(input_width.saturating_sub(visible_width(&input_line)));
        let content = format!("{input_line}{input_padding}{result_text}");

        let mut previous_button = format!("\u{2191} {previous_key}");
        let mut next_button = format!("\u{2193} {next_key}");
        let mut separator = " \u{b7} ".to_string();
        let outer_gap_width = 1usize;
        let available_controls_width = inner_width.saturating_sub(outer_gap_width * 2).saturating_sub(1);
        let mut controls_width = visible_width(&previous_button) + visible_width(&separator) + visible_width(&next_button);
        if controls_width > available_controls_width {
            previous_button = "\u{2191}".to_string();
            next_button = "\u{2193}".to_string();
            separator = " ".to_string();
            controls_width = visible_width(&previous_button) + visible_width(&separator) + visible_width(&next_button);
        }
        let show_buttons = controls_width <= available_controls_width;
        let rendered_buttons = if show_buttons {
            format!(
                "{}{separator}{}",
                (self.navigation_button_style)(&previous_button, self.hovered_navigation_direction == Some(-1)),
                (self.navigation_button_style)(&next_button, self.hovered_navigation_direction == Some(1)),
            )
        } else {
            String::new()
        };
        let outer_gaps_width = if show_buttons { outer_gap_width * 2 } else { 0 };
        let right_rule_width = if !rendered_buttons.is_empty() && inner_width > controls_width + outer_gaps_width { 1 } else { 0 };
        let left_rule_width = inner_width
            .saturating_sub(if show_buttons { controls_width } else { 0 })
            .saturating_sub(outer_gaps_width)
            .saturating_sub(right_rule_width);
        let previous_start = 1 + left_rule_width as i64 + outer_gap_width as i64;
        self.previous_button_start = if show_buttons { previous_start } else { -1 };
        self.previous_button_end = if show_buttons { previous_start + visible_width(&previous_button) as i64 } else { -1 };
        self.next_button_start = if show_buttons { self.previous_button_end + visible_width(&separator) as i64 } else { -1 };
        self.next_button_end = if show_buttons { self.next_button_start + visible_width(&next_button) as i64 } else { -1 };

        if safe_width == 1 {
            return vec!["\u{250c}".to_string(), "\u{2502}".to_string(), "\u{2514}".to_string()];
        }
        vec![
            format!("\u{250c}{}\u{2510}", "\u{2500}".repeat(inner_width)),
            format!("\u{2502}{content}\u{2502}"),
            format!(
                "\u{2514}{}{}{}{}\u{2518}",
                "\u{2500}".repeat(left_rule_width),
                if !rendered_buttons.is_empty() { " " } else { "" },
                rendered_buttons,
                if !rendered_buttons.is_empty() { " " } else { "" }
            ) + &"\u{2500}".repeat(right_rule_width),
        ]
    }
}
