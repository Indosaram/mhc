//! Neon border for the prompt editor while the draft names an ulw-family keyword
//! (`ulw`, `ulw-loop`, `ulw-plan`, `ultrawork`, ...).
//!
//! maho-only presentation (no senpi counterpart).

use maho_tui::utils::strip_terminal_sequences;
use unicode_segmentation::UnicodeSegmentation;

use crate::theme::{ColorMode, palette::rgb_to_256};

const SPARKLE_CELLS_PER_SECOND: f64 = 36.0;
const SPARKLE_TRAIL_CELLS: f64 = 14.0;
const GRADIENT_DRIFT_PERIOD_MS: f64 = 4000.0;

pub fn has_ulw_keyword(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    ["ultrawork", "ulw"].iter().any(|keyword| {
        lower.match_indices(keyword).any(|(index, _)| {
            index == 0 || !lower[..index].chars().next_back().is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
        })
    })
}

pub fn apply_neon_border(lines: &mut [String], now_ms: f64, mode: ColorMode) -> bool {
    let Some(bottom) = lines.iter().skip(1).position(|line| strip_terminal_sequences(line).starts_with('─')).map(|index| index + 1) else {
        return false;
    };
    let top_cells: Vec<String> = strip_terminal_sequences(&lines[0]).graphemes(true).map(str::to_owned).collect();
    let bottom_cells: Vec<String> = strip_terminal_sequences(&lines[bottom]).graphemes(true).map(str::to_owned).collect();
    if top_cells.is_empty() || bottom_cells.is_empty() {
        return false;
    }
    let perimeter = top_cells.len() + bottom_cells.len();
    let top_len = top_cells.len();
    let bottom_len = bottom_cells.len();
    lines[0] = paint(&top_cells, |column| column, perimeter, now_ms, mode);
    // Clockwise loop: top border left->right, then bottom border right->left.
    lines[bottom] = paint(&bottom_cells, |column| top_len + (bottom_len - 1 - column), perimeter, now_ms, mode);
    true
}

fn paint(cells: &[String], loop_index: impl Fn(usize) -> usize, perimeter: usize, now_ms: f64, mode: ColorMode) -> String {
    let mut out = String::new();
    for (column, cell) in cells.iter().enumerate() {
        let (rgb, bold) = neon_cell(loop_index(column), perimeter, now_ms);
        if bold {
            out.push_str("\x1b[1m");
        }
        out.push_str(&fg(rgb, mode));
        out.push_str(cell);
        if bold {
            out.push_str("\x1b[22m");
        }
    }
    out.push_str("\x1b[39m");
    out
}

fn fg([r, g, b]: [u8; 3], mode: ColorMode) -> String {
    match mode {
        ColorMode::Truecolor => format!("\x1b[38;2;{r};{g};{b}m"),
        ColorMode::Color256 => format!("\x1b[38;5;{}m", rgb_to_256([r, g, b])),
    }
}

pub fn neon_cell(index: usize, perimeter: usize, now_ms: f64) -> ([u8; 3], bool) {
    let perimeter_f = perimeter.max(1) as f64;
    let position = index as f64;
    let head = (now_ms.max(0.0) / 1000.0 * SPARKLE_CELLS_PER_SECOND).rem_euclid(perimeter_f);
    // Distance behind the nearer of two heads half a loop apart.
    let behind = [head, (head + perimeter_f / 2.0).rem_euclid(perimeter_f)]
        .into_iter()
        .map(|head| (head - position).rem_euclid(perimeter_f))
        .fold(f64::INFINITY, f64::min);
    let glow = if behind < SPARKLE_TRAIL_CELLS { 1.0 - behind / SPARKLE_TRAIL_CELLS } else { 0.0 };
    let phase = position / perimeter_f + now_ms.max(0.0) / GRADIENT_DRIFT_PERIOD_MS;
    let hue = 185.0 + 125.0 * (0.5 + 0.5 * (phase * std::f64::consts::TAU).sin());
    let saturation = 0.9 - 0.75 * glow * glow;
    let value = 0.55 + 0.45 * glow;
    (hsv_to_rgb(hue, saturation, value), behind < 1.0)
}

fn hsv_to_rgb(hue: f64, saturation: f64, value: f64) -> [u8; 3] {
    let chroma = value * saturation;
    let sector = (hue.rem_euclid(360.0)) / 60.0;
    let x = chroma * (1.0 - (sector.rem_euclid(2.0) - 1.0).abs());
    let (r, g, b) = match sector as u8 {
        0 => (chroma, x, 0.0),
        1 => (x, chroma, 0.0),
        2 => (0.0, chroma, x),
        3 => (0.0, x, chroma),
        4 => (x, 0.0, chroma),
        _ => (chroma, 0.0, x),
    };
    let m = value - chroma;
    let channel = |c: f64| ((c + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    [channel(r), channel(g), channel(b)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_ulw_family_keywords_at_word_starts() {
        for text in ["ulw fix it", "please ULW", "ulw-loop go", "/ulw-plan", "ultrawork mode", "do it (ulw)"] {
            assert!(has_ulw_keyword(text), "{text}");
        }
        for text in ["", "bulwark", "fix the bug", "ultra work"] {
            assert!(!has_ulw_keyword(text), "{text}");
        }
    }

    #[test]
    fn recolors_only_the_two_borders_and_keeps_their_glyphs() {
        let mut lines = vec!["\x1b[38;5;8m──────────\x1b[39m".to_owned(), "❯ ulw hi  ".to_owned(), "\x1b[38;5;8m──────────\x1b[39m".to_owned(), "  → /ulw-loop".to_owned()];
        assert!(apply_neon_border(&mut lines, 0.0, ColorMode::Truecolor));
        assert_eq!(strip_terminal_sequences(&lines[0]), "─".repeat(10));
        assert_eq!(strip_terminal_sequences(&lines[2]), "─".repeat(10));
        assert!(lines[0].contains("\x1b[38;2;") && !lines[0].contains("\x1b[38;5;8m"));
        assert_eq!(lines[1], "❯ ulw hi  ");
        assert_eq!(lines[3], "  → /ulw-loop");
    }

    #[test]
    fn missing_bottom_border_leaves_lines_untouched() {
        let mut lines = vec!["────".to_owned(), "❯ ulw".to_owned()];
        assert!(!apply_neon_border(&mut lines, 0.0, ColorMode::Truecolor));
        assert_eq!(lines, vec!["────".to_owned(), "❯ ulw".to_owned()]);
    }

    #[test]
    fn sparkle_head_flows_around_the_loop_over_time() {
        let perimeter = 80;
        let heads = |now_ms: f64| (0..perimeter).filter(|index| neon_cell(*index, perimeter, now_ms).1).collect::<Vec<_>>();
        let first = heads(0.0);
        let later = heads(500.0);
        assert_eq!(first.len(), 2);
        assert_eq!(later.len(), 2);
        assert_ne!(first, later);
        assert_eq!(later, first.iter().map(|index| (index + 18) % perimeter).collect::<Vec<_>>());
    }

    #[test]
    fn color256_mode_emits_palette_sequences() {
        let mut lines = vec!["───".to_owned(), "───".to_owned()];
        assert!(apply_neon_border(&mut lines, 0.0, ColorMode::Color256));
        assert!(lines[0].contains("\x1b[38;5;") && !lines[0].contains("\x1b[38;2;"));
    }
}
