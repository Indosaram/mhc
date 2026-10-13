//! Neon border for the prompt editor while the draft names an ulw-family keyword
//! (`ulw`, `ulw-loop`, `ulw-plan`, `ultrawork`, ...) or an ulw turn is still running.
//!
//! maho-only presentation (no senpi counterpart).

use maho_tui::utils::strip_terminal_sequences;
use unicode_segmentation::UnicodeSegmentation;

use crate::theme::{ColorMode, palette::rgb_to_256};

const BREATH_SECONDS: f64 = 4.0;
const BASE_LEVEL: f64 = 0.62;
const PEAK_LEVEL: f64 = 1.0;
const PEAK_LIFT: f64 = 0.4;
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
    // The static gradient runs clockwise: top border left->right, then bottom border right->left.
    lines[bottom] = paint(&bottom_cells, |column| (top_len + (bottom_len - 1 - column)) as f64 / perimeter, seconds, mode);
    if bottom > 1 {
        color_prompt_glyph(&mut lines[1], breathe_color(0.0, seconds), mode);
    }
    true
}

fn paint(cells: &[String], position: impl Fn(usize) -> f64, seconds: f64, mode: ColorMode) -> String {
    let mut out = String::new();
    for (column, cell) in cells.iter().enumerate() {
        out.push_str(&fg(breathe_color(position(column), seconds), mode));
        out.push_str(if cell == "─" { "━" } else { cell });
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

/// Color at gradient position `s` (0..1, clockwise from the top-left corner) after `seconds`. Nothing
/// travels: every cell shares one breath level, so the whole border brightens and dims together
/// over the cyan-violet-magenta gradient, which stays in place.
pub fn breathe_color(s: f64, seconds: f64) -> [u8; 3] {
    let breath = breath_level(seconds);
    let level = BASE_LEVEL + (PEAK_LEVEL - BASE_LEVEL) * breath;
    let lift = PEAK_LIFT * breath * breath;
    palette(s).map(|c| ((c + (255.0 - c) * lift) * level).round().clamp(0.0, 255.0) as u8)
}

/// 0 at the bottom of each breath, 1 at its peak; a smooth cosine every `BREATH_SECONDS`.
pub fn breath_level(seconds: f64) -> f64 {
    0.5 - 0.5 * (std::f64::consts::TAU * seconds.max(0.0) / BREATH_SECONDS).cos()
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
    fn thickens_borders_and_recolors_prompt() {
        let mut lines = vec!["\x1b[38;5;8m".to_owned() + &"─".repeat(40) + "\x1b[39m", "\x1b[38;2;1;2;3m❯\x1b[39m ulw hi".to_owned(), "─".repeat(37) + "↓ 2", "  → /ulw-loop".to_owned()];
        assert!(apply_neon_border(&mut lines, 0.0, ColorMode::Truecolor));
        assert_eq!(strip_terminal_sequences(&lines[0]), "━".repeat(40));
        assert_eq!(strip_terminal_sequences(&lines[2]), "━".repeat(37) + "↓ 2");
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

    #[test]
    fn whole_border_breathes_together_every_four_seconds() {
        for s in [0.0, 0.2, 0.5, 0.9] {
            assert!(brightness(breathe_color(s, 2.0)) > brightness(breathe_color(s, 0.0)), "cell {s} brightens toward the peak");
            assert_eq!(breathe_color(s, 0.7), breathe_color(s, 4.7), "cell {s} repeats every breath");
        }
        let ratio = |s: f64, seconds: f64| f64::from(brightness(breathe_color(s, seconds))) / f64::from(brightness(breathe_color(s, 0.0)));
        assert!((ratio(0.4, 1.3) - ratio(0.8, 1.3)).abs() < 0.03, "no position travels: cells rise in step");
    }

    #[test]
    fn breath_is_smooth_frame_to_frame() {
        let step = (0..240).map(|frame| {
            let [a, b] = [frame, frame + 1].map(|f| breathe_color(0.3, f64::from(f) / 60.0));
            (0..3).map(|i| a[i].abs_diff(b[i])).max().unwrap_or(0)
        }).max().unwrap_or(0);
        assert!(step <= 3, "largest per-frame channel step at 60fps: {step}");
    }

    #[test]
    fn color256_mode_emits_palette_sequences() {
        let mut lines = vec!["───".to_owned(), "───".to_owned()];
        assert!(apply_neon_border(&mut lines, 0.0, ColorMode::Color256));
        assert!(lines[0].contains("\x1b[38;5;") && !lines[0].contains("\x1b[38;2;"));
    }
}
