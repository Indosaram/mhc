//! Neon border for the prompt editor while the draft names an ulw-family keyword
//! (`ulw`, `ulw-loop`, `ulw-plan`, `ultrawork`, ...).
//!
//! maho-only presentation (no senpi counterpart).

use maho_tui::utils::strip_terminal_sequences;
use unicode_segmentation::UnicodeSegmentation;

use crate::theme::{ColorMode, palette::rgb_to_256};

const LOOP_SECONDS: f64 = 3.0;
const BANDS: f64 = 2.0;
const HUE_LOOP_SECONDS: f64 = 6.0;
const BASE_LEVEL: f64 = 0.42;
const PEAK_LEVEL: f64 = 0.95;
const PALETTE: [(f64, [f64; 3]); 4] = [
    (0.00, [64.0, 220.0, 230.0]),
    (0.33, [150.0, 110.0, 255.0]),
    (0.66, [240.0, 90.0, 220.0]),
    (1.00, [64.0, 220.0, 230.0]),
];

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
    let (top_len, bottom_len) = (top_cells.len(), bottom_cells.len());
    let perimeter = (top_len + bottom_len) as f64;
    let seconds = now_ms.max(0.0) / 1000.0;
    lines[0] = paint(&top_cells, |column| column as f64 / perimeter, seconds, mode);
    // Clockwise loop: top border left->right, then bottom border right->left.
    lines[bottom] = paint(&bottom_cells, |column| (top_len + (bottom_len - 1 - column)) as f64 / perimeter, seconds, mode);
    if bottom > 1 {
        color_prompt_glyph(&mut lines[1], flow_color(0.0, seconds), mode);
    }
    true
}

fn paint(cells: &[String], position: impl Fn(usize) -> f64, seconds: f64, mode: ColorMode) -> String {
    let mut out = String::new();
    for (column, cell) in cells.iter().enumerate() {
        out.push_str(&fg(flow_color(position(column), seconds), mode));
        out.push_str(cell);
    }
    out.push_str("\x1b[39m");
    out
}

fn color_prompt_glyph(line: &mut String, rgb: [u8; 3], mode: ColorMode) {
    let Some(position) = line.find('❯') else { return };
    if !strip_terminal_sequences(&line[..position]).is_empty() {
        return;
    }
    let rest = &line[position + '❯'.len_utf8()..];
    let rest = rest.strip_prefix("\x1b[39m").unwrap_or(rest);
    *line = format!("\x1b[1m{}❯\x1b[22m\x1b[39m{rest}", fg(rgb, mode));
}

fn fg([r, g, b]: [u8; 3], mode: ColorMode) -> String {
    match mode {
        ColorMode::Truecolor => format!("\x1b[38;2;{r};{g};{b}m"),
        ColorMode::Color256 => format!("\x1b[38;5;{}m", rgb_to_256([r, g, b])),
    }
}

/// Color at loop position `s` (0..1, clockwise from the top-left corner) after `seconds`: wide soft
/// brightness bands travel clockwise while the cyan-violet-magenta palette drifts more slowly.
pub fn flow_color(s: f64, seconds: f64) -> [u8; 3] {
    let soft = band_glow(s, seconds);
    let level = BASE_LEVEL + (PEAK_LEVEL - BASE_LEVEL) * soft;
    let lift = 0.18 * soft * soft;
    palette(s - seconds / HUE_LOOP_SECONDS).map(|c| ((c + (255.0 - c) * lift) * level).round().clamp(0.0, 255.0) as u8)
}

pub fn band_glow(s: f64, seconds: f64) -> f64 {
    (0.5 + 0.5 * (std::f64::consts::TAU * (s * BANDS - seconds / LOOP_SECONDS)).cos()).powf(1.6)
}

fn palette(u: f64) -> [f64; 3] {
    let u = u.rem_euclid(1.0);
    for pair in PALETTE.windows(2) {
        let ((a, from), (b, to)) = (pair[0], pair[1]);
        if u <= b {
            let t = (u - a) / (b - a);
            let smooth = t * t * (3.0 - 2.0 * t);
            return [0, 1, 2].map(|i| from[i] + (to[i] - from[i]) * smooth);
        }
    }
    PALETTE[0].1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn brightness(rgb: [u8; 3]) -> u32 {
        rgb.iter().map(|c| u32::from(*c)).sum()
    }

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
    fn recolors_borders_and_prompt_without_changing_glyphs() {
        let mut lines = vec!["\x1b[38;5;8m".to_owned() + &"─".repeat(40) + "\x1b[39m", "\x1b[38;2;1;2;3m❯\x1b[39m ulw hi".to_owned(), "─".repeat(40), "  → /ulw-loop".to_owned()];
        assert!(apply_neon_border(&mut lines, 0.0, ColorMode::Truecolor));
        assert_eq!(strip_terminal_sequences(&lines[0]), "─".repeat(40));
        assert_eq!(strip_terminal_sequences(&lines[2]), "─".repeat(40));
        assert!(lines[0].contains("\x1b[38;2;") && !lines[0].contains("\x1b[38;5;8m"));
        assert_eq!(strip_terminal_sequences(&lines[1]), "❯ ulw hi");
        assert!(!lines[1].contains("1;2;3m"));
        assert_eq!(lines[3], "  → /ulw-loop");
    }

    #[test]
    fn missing_bottom_border_leaves_lines_untouched() {
        let mut lines = vec!["────".to_owned(), "❯ ulw".to_owned()];
        assert!(!apply_neon_border(&mut lines, 0.0, ColorMode::Truecolor));
        assert_eq!(lines, vec!["────".to_owned(), "❯ ulw".to_owned()]);
    }

    fn glow_peaks(seconds: f64) -> Vec<u32> {
        let glow = |i: u32| band_glow(f64::from(i % 200) / 200.0, seconds);
        (0..200).filter(|i| glow(*i) > glow(i + 199) && glow(*i) >= glow(i + 1)).collect()
    }

    #[test]
    fn two_bright_bands_half_a_loop_apart() {
        let peaks = glow_peaks(0.2);
        assert_eq!(peaks.len(), 2, "{peaks:?}");
        assert!((peaks[1] - peaks[0]).abs_diff(100) <= 1, "{peaks:?}");
    }

    #[test]
    fn bands_flow_clockwise_and_complete_a_loop_in_three_seconds() {
        let start = glow_peaks(0.0)[0];
        let later = glow_peaks(0.3)[0];
        assert!(later > start, "the band moves clockwise: {start} -> {later}");
        for s in [0.1, 0.37, 0.8] {
            assert!((band_glow(s, 0.4) - band_glow(s, 3.4)).abs() < 1e-9, "the brightness wave repeats every 3s at {s}");
        }
    }

    #[test]
    fn whole_border_stays_lit_with_gentle_contrast() {
        let levels = (0..100).map(|i| brightness(flow_color(f64::from(i) / 100.0, 1.234))).collect::<Vec<_>>();
        let min = *levels.iter().min().unwrap_or(&0);
        let max = *levels.iter().max().unwrap_or(&0);
        assert!(min > 120, "dimmest cell still glows: {min}");
        assert!(max > min * 2, "a visible band exists: {min}..{max}");
    }

    #[test]
    fn color256_mode_emits_palette_sequences() {
        let mut lines = vec!["───".to_owned(), "───".to_owned()];
        assert!(apply_neon_border(&mut lines, 0.0, ColorMode::Color256));
        assert!(lines[0].contains("\x1b[38;5;") && !lines[0].contains("\x1b[38;2;"));
    }
}
