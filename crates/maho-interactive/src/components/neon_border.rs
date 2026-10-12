//! Neon border for the prompt editor while the draft names an ulw-family keyword
//! (`ulw`, `ulw-loop`, `ulw-plan`, `ultrawork`, ...).
//!
//! maho-only presentation (no senpi counterpart).

use maho_tui::utils::strip_terminal_sequences;
use unicode_segmentation::UnicodeSegmentation;

use crate::theme::{ColorMode, palette::rgb_to_256};

const COMET_CELLS_PER_SECOND: f64 = 30.0;
const COMET_TRAIL_CELLS: f64 = 30.0;
const COMET_HALO_CELLS: f64 = 3.0;
const HUE_DRIFT_PERIOD_MS: f64 = 6000.0;
const BREATH_PERIOD_MS: f64 = 2600.0;
const IGNITION_MS: f64 = 650.0;
const THICK_GLOW: f64 = 0.4;
const BOLD_GLOW: f64 = 0.8;

pub fn has_ulw_keyword(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    ["ultrawork", "ulw"].iter().any(|keyword| {
        lower.match_indices(keyword).any(|(index, _)| {
            index == 0 || !lower[..index].chars().next_back().is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
        })
    })
}

pub fn apply_neon_border(lines: &mut [String], now_ms: f64, armed_ms: f64, mode: ColorMode) -> bool {
    let Some(bottom) = lines.iter().skip(1).position(|line| strip_terminal_sequences(line).starts_with('─')).map(|index| index + 1) else {
        return false;
    };
    let top_cells: Vec<String> = strip_terminal_sequences(&lines[0]).graphemes(true).map(str::to_owned).collect();
    let bottom_cells: Vec<String> = strip_terminal_sequences(&lines[bottom]).graphemes(true).map(str::to_owned).collect();
    if top_cells.is_empty() || bottom_cells.is_empty() {
        return false;
    }
    let (top_len, bottom_len) = (top_cells.len(), bottom_cells.len());
    let perimeter = top_len + bottom_len;
    lines[0] = paint(&top_cells, |column| column, perimeter, now_ms, armed_ms, mode);
    // Clockwise loop: top border left->right, then bottom border right->left.
    lines[bottom] = paint(&bottom_cells, |column| top_len + (bottom_len - 1 - column), perimeter, now_ms, armed_ms, mode);
    if bottom > 1 {
        glow_prompt_glyph(&mut lines[1], now_ms, armed_ms, mode);
    }
    true
}

fn paint(cells: &[String], loop_index: impl Fn(usize) -> usize, perimeter: usize, now_ms: f64, armed_ms: f64, mode: ColorMode) -> String {
    let mut out = String::new();
    for (column, cell) in cells.iter().enumerate() {
        let frame = glow_cell(loop_index(column), perimeter, now_ms, armed_ms);
        let glyph = if cell == "─" && frame.glow >= THICK_GLOW { "━" } else { cell.as_str() };
        if frame.glow >= BOLD_GLOW {
            out.push_str("\x1b[1m");
        }
        out.push_str(&fg(frame.rgb, mode));
        out.push_str(glyph);
        if frame.glow >= BOLD_GLOW {
            out.push_str("\x1b[22m");
        }
    }
    out.push_str("\x1b[39m");
    out
}

fn glow_prompt_glyph(line: &mut String, now_ms: f64, armed_ms: f64, mode: ColorMode) {
    let Some(position) = line.find('❯') else { return };
    if !strip_terminal_sequences(&line[..position]).is_empty() {
        return;
    }
    let rest = &line[position + '❯'.len_utf8()..];
    let rest = rest.strip_prefix("\x1b[39m").unwrap_or(rest);
    let pulse = 0.5 + 0.5 * (now_ms.max(0.0) / BREATH_PERIOD_MS * std::f64::consts::TAU * 1.5).sin();
    let ignition = ignition(armed_ms);
    let hue = drift_hue(0.0, now_ms);
    let rgb = mix(hsv_to_rgb(hue, 0.85, 0.75 + 0.25 * pulse), [255, 255, 255], 0.35 * pulse * ignition);
    *line = format!("\x1b[1m{}❯\x1b[22m\x1b[39m{rest}", fg(scale(rgb, 0.35 + 0.65 * ignition), mode));
}

fn fg([r, g, b]: [u8; 3], mode: ColorMode) -> String {
    match mode {
        ColorMode::Truecolor => format!("\x1b[38;2;{r};{g};{b}m"),
        ColorMode::Color256 => format!("\x1b[38;5;{}m", rgb_to_256([r, g, b])),
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GlowCell {
    pub rgb: [u8; 3],
    pub glow: f64,
}

pub fn glow_cell(index: usize, perimeter: usize, now_ms: f64, armed_ms: f64) -> GlowCell {
    let perimeter_f = perimeter.max(1) as f64;
    let position = index as f64;
    let now = now_ms.max(0.0);
    let hue = drift_hue(position / perimeter_f, now);
    let breath = 0.5 + 0.5 * (now / BREATH_PERIOD_MS * std::f64::consts::TAU).sin();
    let base = hsv_to_rgb(hue, 0.9, 0.42 + 0.14 * breath);

    let head = (now / 1000.0 * COMET_CELLS_PER_SECOND).rem_euclid(perimeter_f);
    let mut glow: f64 = 0.0;
    for head in [head, (head + perimeter_f / 2.0).rem_euclid(perimeter_f)] {
        let behind = (head - position).rem_euclid(perimeter_f);
        let ahead = (position - head).rem_euclid(perimeter_f);
        let trail = if behind <= COMET_TRAIL_CELLS { (1.0 - behind / COMET_TRAIL_CELLS).powf(2.2) } else { 0.0 };
        let halo = (-(ahead / COMET_HALO_CELLS).powi(2)).exp();
        glow = glow.max(trail.max(halo));
    }
    let lit = mix(hsv_to_rgb(hue + 25.0, 0.55, 1.0), [255, 255, 255], ((glow - 0.75) / 0.25).clamp(0.0, 1.0) * 0.85);
    let mut rgb = mix(base, lit, glow.powf(0.55));

    // Ignition: a white-hot front sweeps both ways from the top-left corner; unreached cells stay dark.
    let distance = position.min(perimeter_f - position);
    let reach = (armed_ms.max(0.0) / IGNITION_MS) * (perimeter_f / 2.0 + 1.0);
    if reach <= perimeter_f / 2.0 + 1.0 {
        let gap = reach - distance;
        if gap < 0.0 {
            return GlowCell { rgb: scale(base, 0.25), glow: 0.0 };
        }
        let front = (-(gap / 1.5).powi(2)).exp();
        rgb = mix(rgb, [255, 255, 255], front);
        glow = glow.max(front);
    }
    GlowCell { rgb, glow }
}

fn ignition(armed_ms: f64) -> f64 {
    (armed_ms.max(0.0) / IGNITION_MS).clamp(0.0, 1.0)
}

fn drift_hue(phase: f64, now_ms: f64) -> f64 {
    let t = phase + now_ms / HUE_DRIFT_PERIOD_MS;
    185.0 + 135.0 * (0.5 + 0.5 * (t * std::f64::consts::TAU).sin())
}

fn mix(a: [u8; 3], b: [u8; 3], t: f64) -> [u8; 3] {
    let t = t.clamp(0.0, 1.0);
    [0, 1, 2].map(|i| (f64::from(a[i]) + (f64::from(b[i]) - f64::from(a[i])) * t).round() as u8)
}

fn scale(rgb: [u8; 3], k: f64) -> [u8; 3] {
    rgb.map(|c| (f64::from(c) * k.clamp(0.0, 1.0)).round() as u8)
}

fn hsv_to_rgb(hue: f64, saturation: f64, value: f64) -> [u8; 3] {
    let chroma = value * saturation;
    let sector = hue.rem_euclid(360.0) / 60.0;
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

    const ARMED: f64 = 10_000.0;

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
    fn repaints_borders_with_light_or_heavy_rules_and_keeps_widths() {
        let mut lines = vec!["\x1b[38;5;8m".to_owned() + &"─".repeat(40) + "\x1b[39m", "❯ ulw hi".to_owned(), "─".repeat(40), "  → /ulw-loop".to_owned()];
        assert!(apply_neon_border(&mut lines, 0.0, ARMED, ColorMode::Truecolor));
        for line in [&lines[0], &lines[2]] {
            let plain = strip_terminal_sequences(line);
            assert_eq!(plain.chars().count(), 40);
            assert!(plain.chars().all(|c| c == '─' || c == '━'), "{plain}");
            assert!(plain.contains('━'), "comet cores render the heavy rule");
        }
        assert_eq!(lines[3], "  → /ulw-loop");
    }

    #[test]
    fn prompt_glyph_glows_and_keeps_the_draft() {
        let mut lines = vec!["─".repeat(10), "\x1b[38;2;1;2;3m❯\x1b[39m ulw hi".to_owned(), "─".repeat(10)];
        assert!(apply_neon_border(&mut lines, 0.0, ARMED, ColorMode::Truecolor));
        assert_eq!(strip_terminal_sequences(&lines[1]), "❯ ulw hi");
        assert!(lines[1].starts_with("\x1b[1m\x1b[38;2;") && !lines[1].contains("1;2;3m"));
    }

    #[test]
    fn missing_bottom_border_leaves_lines_untouched() {
        let mut lines = vec!["────".to_owned(), "❯ ulw".to_owned()];
        assert!(!apply_neon_border(&mut lines, 0.0, ARMED, ColorMode::Truecolor));
        assert_eq!(lines, vec!["────".to_owned(), "❯ ulw".to_owned()]);
    }

    #[test]
    fn comet_cores_travel_clockwise_over_time() {
        let perimeter = 120;
        let cores = |now: f64| (0..perimeter).filter(|i| glow_cell(*i, perimeter, now, ARMED).glow > 0.99).collect::<Vec<_>>();
        let first = cores(0.0);
        let later = cores(500.0);
        assert_eq!(first.len(), 2);
        assert_eq!(later, first.iter().map(|i| (i + 15) % perimeter).collect::<Vec<_>>());
    }

    #[test]
    fn trail_fades_behind_the_core_and_halo_is_short() {
        let perimeter = 200;
        let now = 1000.0;
        let glow = |i: usize| glow_cell(i, perimeter, now, ARMED).glow;
        assert!(glow(30) > glow(25) && glow(25) > glow(20) && glow(20) > glow(14));
        assert!(glow(31) < glow(29), "the halo ahead is softer than the trail behind");
        assert!(glow(60) < 0.01, "cells far from both comets only carry the base");
    }

    #[test]
    fn ignition_sweeps_out_from_the_top_left_corner() {
        let perimeter = 100;
        let lit = |armed: f64| (0..perimeter).filter(|i| glow_cell(*i, perimeter, 7_777.0, armed).rgb != [0, 0, 0] && glow_cell(*i, perimeter, 7_777.0, armed).rgb.iter().map(|c| u32::from(*c)).sum::<u32>() > 60).count();
        assert!(lit(50.0) < lit(300.0) && lit(300.0) < lit(ARMED));
        assert!(glow_cell(perimeter / 2, perimeter, 7_777.0, 50.0).glow == 0.0, "the far side is still dark right after arming");
    }

    #[test]
    fn color256_mode_emits_palette_sequences() {
        let mut lines = vec!["───".to_owned(), "───".to_owned()];
        assert!(apply_neon_border(&mut lines, 0.0, ARMED, ColorMode::Color256));
        assert!(lines[0].contains("\x1b[38;5;") && !lines[0].contains("\x1b[38;2;"));
    }
}
