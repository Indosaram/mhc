//! Port of `components/visual-truncate.ts`.
use maho_tui::components::text::Text;
use maho_tui::tui::Component;

pub struct VisualTruncateResult {
    pub visual_lines: Vec<String>,
    pub skipped_count: usize,
}

pub fn truncate_to_visual_lines(
    text: &str,
    max_visual_lines: usize,
    width: usize,
    padding_x: usize,
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
    let visual_lines = all_visual_lines.split_off(skipped_count);
    VisualTruncateResult { visual_lines, skipped_count }
}
