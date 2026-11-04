//! Port of senpi `packages/coding-agent/src/modes/interactive/theme/reserved-keyword-rainbow.ts`:
//! the per-character spectrum and the cached foreground styler. senpi's `find` half delegates to
//! core `reserved-keywords.ts` (`findReservedKeywordRuns`), which is not ported in this crate, so
//! [`ReservedKeywordHighlight`] exposes only `style`.

use std::cell::RefCell;
use std::collections::HashMap;

use maho_tui::colors::{
    Color, ColorInput, ColorMixSpace, TerminalColorMode, foreground_ansi, mix_colors, parse_color,
};

use super::theme::{ColorMode, Theme};
use super::theme_json::ThemeAppearance;

struct SpectrumStop {
    at: f64,
    color: Color,
}

const POSITIONS: [f64; 7] = [0.0, 0.18, 0.36, 0.54, 0.72, 0.9, 1.0];

// The desktop composer's `.composer-reserved-keyword` stops: 700 shades on light surfaces,
// 400 shades on dark ones, so every stop clears 4.5:1 as text.
const LIGHT: [&str; 7] = [
    "#b91c1c", "#c2410c", "#15803d", "#0f766e", "#1d4ed8", "#be185d", "#b91c1c",
];
const DARK: [&str; 7] = [
    "#f87171", "#fbbf24", "#4ade80", "#2dd4bf", "#60a5fa", "#f472b6", "#f87171",
];

// A run spans red through pink; the final stop wraps back to red and is left out so the first
// and last characters of a keyword never share a colour.
const SPAN_END: f64 = 0.9;

fn white() -> Color {
    Color::Rgb {
        r: 255.0,
        g: 255.0,
        b: 255.0,
    }
}

fn spectrum(appearance: ThemeAppearance) -> Vec<SpectrumStop> {
    let hexes = match appearance {
        ThemeAppearance::Light => LIGHT,
        ThemeAppearance::Dark => DARK,
    };
    hexes
        .iter()
        .zip(POSITIONS)
        .map(|(hex, at)| SpectrumStop {
            at,
            color: parse_color(ColorInput::Text(hex)).unwrap_or_else(|_| white()),
        })
        .collect()
}

pub fn reserved_keyword_color(appearance: ThemeAppearance, index: usize, length: usize) -> Color {
    let stops = spectrum(appearance);
    let at = if length <= 1 {
        0.0
    } else {
        (index.min(length - 1) as f64 / (length - 1) as f64) * SPAN_END
    };
    for pair in stops.windows(2) {
        let (left, right) = (&pair[0], &pair[1]);
        if at > right.at {
            continue;
        }
        let amount = if right.at == left.at {
            0.0
        } else {
            (at - left.at) / (right.at - left.at)
        };
        return mix_colors(left.color, right.color, amount, ColorMixSpace::Srgb)
            .unwrap_or(left.color);
    }
    stops.first().map_or_else(white, |stop| stop.color)
}

/// senpi `ReservedKeywordHighlightSource`.
pub struct ReservedKeywordHighlightSource {
    pub appearance: Box<dyn Fn() -> ThemeAppearance>,
    pub color_mode: Box<dyn Fn() -> TerminalColorMode>,
    /// senpi `env.NO_COLOR`; `None` reads the process environment.
    pub no_color: Option<Option<String>>,
}

/// The `style` half of senpi's `EditorKeywordHighlight`.
pub struct ReservedKeywordHighlight {
    source: ReservedKeywordHighlightSource,
    cache: RefCell<HashMap<(bool, bool, usize, usize), String>>,
}

impl ReservedKeywordHighlight {
    pub fn style(&self, text: &str, index: usize, length: usize) -> String {
        let appearance = (self.source.appearance)();
        let mode = (self.source.color_mode)();
        let key = (
            appearance == ThemeAppearance::Light,
            mode == TerminalColorMode::Truecolor,
            index,
            length,
        );
        let open = self
            .cache
            .borrow_mut()
            .entry(key)
            .or_insert_with(|| {
                foreground_ansi(reserved_keyword_color(appearance, index, length), mode)
            })
            .clone();
        format!("{open}{text}\x1b[39m")
    }
}

/// `NO_COLOR` (any non-empty value) turns the decoration off entirely.
pub fn create_reserved_keyword_highlight(
    source: ReservedKeywordHighlightSource,
) -> Option<ReservedKeywordHighlight> {
    let no_color = match &source.no_color {
        Some(value) => value.clone(),
        None => std::env::var("NO_COLOR").ok(),
    };
    if no_color.is_some_and(|value| !value.is_empty()) {
        return None;
    }
    Some(ReservedKeywordHighlight {
        source,
        cache: RefCell::new(HashMap::new()),
    })
}

/// senpi `getReservedKeywordHighlight()` (theme.ts): the rainbow for the editor, following the
/// theme's colour mode. The port's `Theme` carries no detected appearance, so a theme without an
/// explicit one paints with the dark spectrum.
pub fn get_reserved_keyword_highlight(
    theme: &Theme,
) -> Option<maho_tui::components::editor::EditorKeywordHighlight> {
    let appearance = theme.appearance.unwrap_or(ThemeAppearance::Dark);
    let mode = match theme.get_color_mode() {
        ColorMode::Truecolor => TerminalColorMode::Truecolor,
        ColorMode::Color256 => TerminalColorMode::Color256,
    };
    let highlight = std::rc::Rc::new(create_reserved_keyword_highlight(
        ReservedKeywordHighlightSource {
            appearance: Box::new(move || appearance),
            color_mode: Box::new(move || mode),
            no_color: None,
        },
    )?);
    Some(maho_tui::components::editor::EditorKeywordHighlight {
        find: std::rc::Rc::new(|text: &str| {
            maho_core::reserved_keywords::find_reserved_keyword_runs(text)
                .into_iter()
                .map(|run| maho_tui::autocomplete::MentionRange {
                    start: run.start,
                    end: run.end,
                })
                .collect()
        }),
        style: std::rc::Rc::new(move |text, index, length| highlight.style(text, index, length)),
    })
}

#[cfg(test)]
mod tests {
    //! Port of senpi `test/reserved-keyword-rainbow.test.ts` (the `find` case needs core
    //! `reserved-keywords.ts`, which this crate does not port).

    use std::collections::HashSet;

    use maho_tui::colors::color_to_hex;

    use super::*;

    fn highlight(
        appearance: ThemeAppearance,
        mode: TerminalColorMode,
        no_color: Option<&str>,
    ) -> Option<ReservedKeywordHighlight> {
        create_reserved_keyword_highlight(ReservedKeywordHighlightSource {
            appearance: Box::new(move || appearance),
            color_mode: Box::new(move || mode),
            no_color: Some(no_color.map(str::to_string)),
        })
    }

    #[test]
    fn starts_on_the_desktops_red_and_ends_on_its_pink_for_each_appearance() {
        let hex = |appearance, index| color_to_hex(reserved_keyword_color(appearance, index, 8));
        assert_eq!(hex(ThemeAppearance::Light, 0), "#b91c1c");
        assert_eq!(hex(ThemeAppearance::Light, 7), "#be185d");
        assert_eq!(hex(ThemeAppearance::Dark, 0), "#f87171");
        assert_eq!(hex(ThemeAppearance::Dark, 7), "#f472b6");
    }

    #[test]
    fn gives_each_character_of_a_short_keyword_its_own_colour() {
        let colours: HashSet<String> = (0..3)
            .map(|index| color_to_hex(reserved_keyword_color(ThemeAppearance::Dark, index, 3)))
            .collect();
        assert_eq!(colours.len(), 3);
    }

    #[test]
    fn emits_a_truecolor_foreground_per_character_and_resets_only_the_foreground() {
        let styled = highlight(ThemeAppearance::Dark, TerminalColorMode::Truecolor, None)
            .map(|highlight| highlight.style("u", 0, 3));
        assert_eq!(styled.as_deref(), Some("\x1b[38;2;248;113;113mu\x1b[39m"));
    }

    #[test]
    fn falls_back_to_the_256_colour_palette() {
        let styled = highlight(ThemeAppearance::Dark, TerminalColorMode::Color256, None)
            .map(|highlight| highlight.style("u", 0, 3))
            .unwrap_or_default();
        let code = styled
            .strip_prefix("\x1b[38;5;")
            .and_then(|rest| rest.strip_suffix("mu\x1b[39m"))
            .unwrap_or("");
        assert!(
            !code.is_empty() && code.bytes().all(|b| b.is_ascii_digit()),
            "{styled:?}"
        );
    }

    #[test]
    fn is_off_under_no_color_and_on_when_no_color_is_empty() {
        assert!(
            highlight(
                ThemeAppearance::Dark,
                TerminalColorMode::Truecolor,
                Some("1")
            )
            .is_none()
        );
        assert!(
            highlight(
                ThemeAppearance::Dark,
                TerminalColorMode::Truecolor,
                Some("")
            )
            .is_some()
        );
    }
}
