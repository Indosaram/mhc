//! Port of `components/visual-truncate.ts`.
use maho_tui::components::text::Text;
use maho_tui::tui::Component;

pub struct VisualTruncateResult {
    pub visual_lines: Vec<String>,
    pub skipped_count: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VisualLineSelection { Start, End }

pub struct VisualLinePreview {
    text: String,
    max_visual_lines: usize,
    keep: VisualLineSelection,
    format_hint: Box<dyn Fn(usize) -> String>,
    cached_width: Option<usize>,
    cached_lines: Option<Vec<String>>,
}

impl VisualLinePreview {
    pub fn new(text: String, max_visual_lines: usize, keep: VisualLineSelection, format_hint: Box<dyn Fn(usize) -> String>) -> Self {
        Self { text, max_visual_lines, keep, format_hint, cached_width: None, cached_lines: None }
    }
}

impl Component for VisualLinePreview {
    fn render(&mut self, width: usize) -> Vec<String> {
        if self.cached_lines.is_none() || self.cached_width != Some(width) {
            let preview = truncate_to_visual_lines_from(&self.text, self.max_visual_lines, width, 0, self.keep);
            let mut lines = preview.visual_lines;
            if preview.skipped_count > 0 {
                let hint = maho_tui::utils::truncate_to_width(&(self.format_hint)(preview.skipped_count), width, "...");
                match self.keep { VisualLineSelection::Start => lines.push(hint), VisualLineSelection::End => lines.insert(0, hint) }
            }
            self.cached_lines = Some(lines);
            self.cached_width = Some(width);
        }
        self.cached_lines.clone().unwrap_or_default()
    }
    fn invalidate(&mut self) { self.cached_width = None; self.cached_lines = None; }
}

pub fn truncate_to_visual_lines(
    text: &str,
    max_visual_lines: usize,
    width: usize,
    padding_x: usize,
) -> VisualTruncateResult {
    truncate_to_visual_lines_from(text, max_visual_lines, width, padding_x, VisualLineSelection::End)
}

pub fn truncate_to_visual_lines_from(
    text: &str,
    max_visual_lines: usize,
    width: usize,
    padding_x: usize,
    selection: VisualLineSelection,
) -> VisualTruncateResult {
    if text.is_empty() {
        return VisualTruncateResult { visual_lines: Vec::new(), skipped_count: 0 };
    }
    let mut temp_text = Text::with_padding(text, padding_x, 0);
    let mut all_visual_lines = temp_text.render(width);
    if all_visual_lines.len() <= max_visual_lines {
        return VisualTruncateResult { visual_lines: all_visual_lines, skipped_count: 0 };
    }
    let skipped_count = all_visual_lines.len() - max_visual_lines;
    let visual_lines = match selection {
        VisualLineSelection::Start => { all_visual_lines.truncate(max_visual_lines); all_visual_lines }
        VisualLineSelection::End => all_visual_lines.split_off(skipped_count),
    };
    VisualTruncateResult { visual_lines, skipped_count }
}
