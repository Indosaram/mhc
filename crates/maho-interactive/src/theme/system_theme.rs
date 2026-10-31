//! Port of senpi `packages/coding-agent/src/modes/interactive/theme/system-theme.ts`: the `system`
//! theme's colors derived from the terminal's own theme.
//!
//! Every token belongs to a color family (its hue) and has contrast rules: it must reach a contrast
//! level on the background and on the panels it is drawn on. Hue and saturation come from the
//! terminal's palette color for the family's ANSI slot, or from the family's own hue when the
//! terminal reports no palette. Lightness comes from the rules alone. Colors are built in OKHSL and
//! a palette color never gains OKLCH chroma when it moves to another lightness.
//!
//! Tokens are the theme JSON keys (`"text"`, `"selectedBg"`, ...). This module is the generator;
//! registering `system` in the theme list is the theme registry's job.

use std::collections::{BTreeMap, HashMap};
use std::sync::LazyLock;

use maho_tui::colors::{
    Color, color_to_okhsl, color_to_oklch, color_to_rgb, okhsl_color, oklch_color,
};
use maho_tui::oklab::oklab_to_okhsl_lightness;
use maho_tui::terminal_colors::RgbColor;

use super::theme_json::ThemeAppearance;

pub const SYSTEM_THEME_NAME: &str = "system";

/// A family's OKHSL hue and saturation: `max` at mid lightness, falling toward `min` at black and
/// white. `slot` is the ANSI palette slot the family takes its hue and saturation from.
#[derive(Clone, Copy)]
struct Family {
    hue: f64,
    min: f64,
    max: f64,
    slot: usize,
}

const fn family(hue: f64, min: f64, max: f64, slot: usize) -> Family {
    Family {
        hue,
        min,
        max,
        slot,
    }
}

const NEUTRAL: Family = family(231.49, 0.02, 0.08, 8);

fn family_of(name: &str) -> Family {
    match name {
        "blue" => family(231.49, 0.1, 0.68, 4),
        "green" => family(158.68, 0.1, 0.76, 2),
        "red" => family(20.0, 0.1, 0.92, 1),
        "yellow" => family(82.36, 0.5, 1.0, 3),
        "orange" => family(52.0, 0.12, 0.85, 3),
        "violet" => family(295.0, 0.2, 0.6, 5),
        "calamine" => family(202.43, 0.1, 0.74, 6),
        "thinkingSlate" => family(231.49, 0.08, 0.2, 4),
        "thinkingBlue" => family(231.49, 0.2, 0.45, 4),
        "thinkingPeriwinkle" => family(263.25, 0.3, 0.6, 6),
        "thinkingViolet" => family(295.0, 0.4, 0.75, 5),
        "thinkingMagenta" => family(337.5, 0.5, 0.85, 13),
        "thinkingRed" => family(20.0, 0.95, 1.0, 1),
        _ => NEUTRAL,
    }
}

/// senpi `TOKEN_FAMILIES`, in declaration order.
const TOKEN_FAMILIES: &[(&str, &str)] = &[
    ("selectedBg", "blue"),
    ("searchMatchBg", "orange"),
    ("userMessageBg", "blue"),
    ("customMessageBg", "violet"),
    ("toolPendingBg", "neutral"),
    ("toolSuccessBg", "green"),
    ("toolErrorBg", "red"),
    ("text", "neutral"),
    ("userMessageText", "neutral"),
    ("customMessageText", "neutral"),
    ("toolTitle", "neutral"),
    ("syntaxOperator", "neutral"),
    ("syntaxPunctuation", "neutral"),
    ("muted", "neutral"),
    ("dim", "neutral"),
    ("thinkingText", "neutral"),
    ("toolOutput", "neutral"),
    ("mdLinkUrl", "neutral"),
    ("mdQuote", "neutral"),
    ("mdQuoteBorder", "neutral"),
    ("mdHr", "neutral"),
    ("mdCodeBlockBorder", "neutral"),
    ("toolDiffContext", "neutral"),
    ("syntaxComment", "neutral"),
    ("scrollbarTrack", "neutral"),
    ("scrollbarThumb", "neutral"),
    ("searchMatchText", "neutral"),
    ("borderMuted", "neutral"),
    ("accent", "violet"),
    ("borderAccent", "violet"),
    ("customMessageLabel", "violet"),
    ("mdCode", "violet"),
    ("mdListBullet", "violet"),
    ("syntaxType", "violet"),
    ("border", "blue"),
    ("mdLink", "blue"),
    ("skillMention", "blue"),
    ("syntaxKeyword", "blue"),
    ("syntaxVariable", "calamine"),
    ("success", "green"),
    ("mdCodeBlock", "green"),
    ("toolDiffAdded", "green"),
    ("bashMode", "green"),
    ("syntaxNumber", "green"),
    ("error", "red"),
    ("toolDiffRemoved", "red"),
    ("warning", "yellow"),
    ("mdHeading", "yellow"),
    ("syntaxFunction", "yellow"),
    ("syntaxString", "orange"),
    ("thinkingOff", "neutral"),
    ("thinkingMinimal", "thinkingSlate"),
    ("thinkingLow", "thinkingBlue"),
    ("thinkingMedium", "thinkingPeriwinkle"),
    ("thinkingHigh", "thinkingViolet"),
    ("thinkingXhigh", "thinkingMagenta"),
    ("thinkingMax", "thinkingRed"),
];

fn token_family_name(token: &str) -> &'static str {
    TOKEN_FAMILIES
        .iter()
        .find(|(name, _)| *name == token)
        .map_or("neutral", |(_, family)| family)
}

fn token_family(token: &str) -> Family {
    family_of(token_family_name(token))
}

/// Palette slots for tokens that would otherwise share a hue with a similar token.
fn token_slot(token: &str) -> Option<usize> {
    match token {
        "syntaxString" => Some(2),
        "syntaxNumber" => Some(5),
        "searchMatchBg" => Some(3),
        _ => None,
    }
}

/// A target-lightness polynomial in the surface's OKLab lightness; `reachable` is the range of
/// surface lightness where the level can be reached.
struct Curve {
    coefficients: [f64; 6],
    reachable: (f64, f64),
}

const fn curve(coefficients: [f64; 6], reachable: (f64, f64)) -> Curve {
    Curve {
        coefficients,
        reachable,
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Level {
    Panel,
    Track,
    Thinking0,
    Thinking1,
    Thinking2,
    Thinking3,
    Thinking4,
    Thinking5,
    Thinking6,
    Subtle,
    Thumb,
    Readable,
    Emphasis,
    TextOnPanel,
    Text,
}

fn level_curve(level: Level, appearance: ThemeAppearance) -> Curve {
    let dark = appearance == ThemeAppearance::Dark;
    match (level, dark) {
        (Level::Panel, true) => curve(
            [0.29131, -0.39746, 2.33185, -0.85524, -1.2076, 0.86276],
            (0.0, 0.979),
        ),
        (Level::Panel, false) => curve(
            [-3.74073, 27.94549, -78.44258, 112.6798, -79.60015, 22.11277],
            (0.348, 1.0),
        ),
        (Level::Track, true) => curve(
            [0.39028, -0.23015, 0.83573, 2.43829, -4.38292, 2.01582],
            (0.0, 0.946),
        ),
        (Level::Track, false) => curve(
            [
                -5.24921, 38.37322, -107.28833, 152.10005, -106.17127, 29.18061,
            ],
            (0.368, 1.0),
        ),
        (Level::Thinking0, true) => curve(
            [0.52988, -0.05809, -0.30924, 4.63567, -6.52933, 2.89108],
            (0.0, 0.873),
        ),
        (Level::Thinking0, false) => curve(
            [
                -28.27749, 182.85284, -469.62416, 603.15916, -384.59976, 97.35147,
            ],
            (0.51, 1.0),
        ),
        (Level::Thinking1, true) => curve(
            [0.55278, -0.03667, -0.45659, 4.95347, -6.90265, 3.0706],
            (0.0, 0.858),
        ),
        (Level::Thinking1, false) => curve(
            [
                -37.10484, 235.86282, -596.62344, 754.3633, -474.00763, 118.3551,
            ],
            (0.535, 1.0),
        ),
        (Level::Thinking2, true) => curve(
            [0.57486, -0.01765, -0.58987, 5.25227, -7.27175, 3.25532],
            (0.0, 0.842),
        ),
        (Level::Thinking2, false) => curve(
            [
                -59.89653, 377.05024, -945.07843, 1182.03145, -734.96375, 181.68658,
            ],
            (0.556, 1.0),
        ),
        (Level::Thinking3, true) => curve(
            [0.59621, -0.00062, -0.71148, 5.53588, -7.6392, 3.44606],
            (0.0, 0.827),
        ),
        (Level::Thinking3, false) => curve(
            [
                -72.07122,
                445.84082,
                -1099.57352,
                1353.88793,
                -829.53392,
                202.26164,
            ],
            (0.58, 1.0),
        ),
        (Level::Thinking4, true) => curve(
            [0.61691, 0.01462, -0.82288, 5.80651, -8.00641, 3.64333],
            (0.0, 0.811),
        ),
        (Level::Thinking4, false) => curve(
            [
                -110.14338,
                674.21488,
                -1645.75941,
                2004.32367,
                -1215.15899,
                293.3183,
            ],
            (0.6, 1.0),
        ),
        (Level::Thinking5, true) => curve(
            [0.63702, 0.02826, -0.92498, 6.06465, -8.37246, 3.84651],
            (0.0, 0.795),
        ),
        (Level::Thinking5, false) => curve(
            [
                -175.47701,
                1063.54495,
                -2570.70594,
                3098.80776,
                -1860.15527,
                444.76392,
            ],
            (0.62, 1.0),
        ),
        (Level::Thinking6, true) => curve(
            [0.65658, 0.04044, -1.01835, 6.30989, -8.73529, 4.05439],
            (0.0, 0.779),
        ),
        (Level::Thinking6, false) => curve(
            [
                -183.81712,
                1094.70055,
                -2602.68539,
                3088.71276,
                -1826.91131,
                430.75931,
            ],
            (0.643, 1.0),
        ),
        (Level::Subtle, true) => curve(
            [0.56762, -0.02475, -0.5383, 5.12628, -7.10931, 3.17324],
            (0.0, 0.848),
        ),
        (Level::Subtle, false) => curve(
            [
                -232.85459,
                1376.54473,
                -3249.11801,
                3827.91186,
                -2248.29472,
                526.55751,
            ],
            (0.657, 1.0),
        ),
        (Level::Thumb, true) => curve(
            [0.60323, 0.00278, -0.73328, 5.57157, -7.68067, 3.46933],
            (0.0, 0.823),
        ),
        (Level::Thumb, false) => curve(
            [
                -82.89897,
                511.01355,
                -1255.98095,
                1540.76821,
                -940.68087,
                228.58523,
            ],
            (0.586, 1.0),
        ),
        (Level::Readable, true) => curve(
            [0.66937, 0.04704, -1.06871, 6.43941, -8.9332, 4.17229],
            (0.0, 0.77),
        ),
        (Level::Readable, false) => curve(
            [
                -1554.52576,
                8733.56817,
                -19604.93507,
                21977.72696,
                -12300.99599,
                2749.81288,
            ],
            (0.751, 1.0),
        ),
        (Level::Emphasis, true) => curve(
            [0.7303, 0.07695, -1.31626, 7.1681, -10.14436, 4.92846],
            (0.0, 0.712),
        ),
        (Level::Emphasis, false) => curve(
            [
                -4948.31942,
                26870.91986,
                -58334.48399,
                63280.17197,
                -34298.01053,
                7430.30146,
            ],
            (0.811, 1.0),
        ),
        (Level::TextOnPanel, true) => curve(
            [0.86713, 0.05232, -0.89428, 4.79014, -5.5432, 1.75023],
            (0.0, 0.542),
        ),
        (Level::TextOnPanel, false) => curve(
            [
                -8570.89457,
                43954.60805,
                -90084.00702,
                92220.6791,
                -47152.15802,
                9632.27113,
            ],
            (0.867, 1.0),
        ),
        (Level::Text, true) => curve(
            [0.89242, 0.02311, -0.44862, 2.34417, -0.06084, -2.63844],
            (0.0, 0.5),
        ),
        (Level::Text, false) => curve(
            [
                -2004.67048,
                6664.47299,
                -6060.70202,
                -1792.61209,
                5133.82359,
                -1939.85583,
            ],
            (0.894, 1.0),
        ),
    }
}

struct Rule {
    token: &'static str,
    on: Vec<&'static str>,
    level: Level,
}

const TOOL_PANELS: [&str; 3] = ["toolPendingBg", "toolSuccessBg", "toolErrorBg"];
const MESSAGE_PANELS: [&str; 2] = ["userMessageBg", "customMessageBg"];
const PANELS: [&str; 7] = [
    "userMessageBg",
    "toolPendingBg",
    "toolSuccessBg",
    "toolErrorBg",
    "selectedBg",
    "searchMatchBg",
    "customMessageBg",
];
const THINKING: [&str; 7] = [
    "thinkingOff",
    "thinkingMinimal",
    "thinkingLow",
    "thinkingMedium",
    "thinkingHigh",
    "thinkingXhigh",
    "thinkingMax",
];
const THINKING_LEVELS: [Level; 7] = [
    Level::Thinking0,
    Level::Thinking1,
    Level::Thinking2,
    Level::Thinking3,
    Level::Thinking4,
    Level::Thinking5,
    Level::Thinking6,
];

fn surfaces(parts: &[&[&'static str]]) -> Vec<&'static str> {
    parts.iter().flat_map(|part| part.iter().copied()).collect()
}

static RULES: LazyLock<Vec<Rule>> = LazyLock::new(|| {
    let mut rules = Vec::new();
    let mut each = |tokens: &[&'static str], on: Vec<&'static str>, level: Level| {
        for token in tokens {
            rules.push(Rule {
                token: *token,
                on: on.clone(),
                level,
            });
        }
    };
    let bg: &[&str] = &["background"];
    each(&PANELS, bg.to_vec(), Level::Panel);
    each(&["text"], bg.to_vec(), Level::Text);
    each(&["text"], vec!["selectedBg"], Level::TextOnPanel);
    each(
        &["userMessageText"],
        vec!["userMessageBg"],
        Level::TextOnPanel,
    );
    each(&["toolTitle"], TOOL_PANELS.to_vec(), Level::TextOnPanel);
    each(
        &["accent", "success", "error", "warning"],
        surfaces(&[&["background", "selectedBg"], &TOOL_PANELS]),
        Level::Readable,
    );
    let with_custom = surfaces(&[
        &["background", "selectedBg", "customMessageBg"],
        &TOOL_PANELS,
    ]);
    each(&["muted"], with_custom.clone(), Level::Readable);
    each(&["dim"], with_custom, Level::Subtle);
    each(&["thinkingText"], bg.to_vec(), Level::Readable);
    each(
        &["customMessageText"],
        surfaces(&[&["customMessageBg"], &TOOL_PANELS]),
        Level::Readable,
    );
    each(
        &["customMessageLabel"],
        surfaces(&[
            &["background", "customMessageBg", "selectedBg"],
            &TOOL_PANELS,
        ]),
        Level::Readable,
    );
    each(
        &["toolOutput"],
        surfaces(&[bg, &TOOL_PANELS]),
        Level::Readable,
    );
    each(
        &[
            "mdHeading",
            "mdLink",
            "mdLinkUrl",
            "mdCode",
            "mdQuote",
            "mdCodeBlockBorder",
            "mdListBullet",
            "skillMention",
        ],
        surfaces(&[bg, &MESSAGE_PANELS]),
        Level::Readable,
    );
    let everywhere = surfaces(&[bg, &MESSAGE_PANELS, &TOOL_PANELS]);
    each(&["mdCodeBlock"], everywhere.clone(), Level::Readable);
    each(
        &["toolDiffAdded", "toolDiffRemoved", "toolDiffContext"],
        surfaces(&[bg, &TOOL_PANELS]),
        Level::Readable,
    );
    each(
        &[
            "syntaxComment",
            "syntaxKeyword",
            "syntaxFunction",
            "syntaxVariable",
            "syntaxString",
            "syntaxNumber",
            "syntaxType",
            "syntaxOperator",
            "syntaxPunctuation",
        ],
        everywhere.clone(),
        Level::Readable,
    );
    each(&["searchMatchText"], vec!["searchMatchBg"], Level::Readable);
    each(
        &["bashMode", "border", "borderAccent"],
        bg.to_vec(),
        Level::Readable,
    );
    each(&["borderMuted"], bg.to_vec(), Level::Subtle);
    each(&["mdQuoteBorder", "mdHr"], everywhere, Level::Readable);
    each(&["scrollbarTrack"], bg.to_vec(), Level::Track);
    each(&["scrollbarThumb"], vec!["scrollbarTrack"], Level::Thumb);
    for (token, level) in THINKING.iter().zip(THINKING_LEVELS) {
        each(&[*token], bg.to_vec(), level);
    }
    rules
});

/// Relaxation compresses levels stronger than this one toward it before weakening all levels.
fn readable_floor(appearance: ThemeAppearance) -> Level {
    match appearance {
        ThemeAppearance::Dark => Level::Readable,
        ThemeAppearance::Light => Level::Subtle,
    }
}

/// Body text uses the terminal's foreground when it reaches this level.
const FOREGROUND_LEVEL: Level = Level::Emphasis;
/// Text-level tokens that take the terminal's foreground.
const FOREGROUND_TOKENS: [&str; 3] = ["text", "userMessageText", "toolTitle"];
/// WCAG 2 contrast ratio that body text must reach on the surfaces it is drawn on.
const TEXT_MINIMUM_WCAG_CONTRAST: f64 = 4.5;

/// Tokens in dependency order: every surface before the tokens drawn on it.
static SOLVE_ORDER: LazyLock<Vec<&'static str>> = LazyLock::new(|| {
    fn visit(token: &'static str, order: &mut Vec<&'static str>) {
        if order.contains(&token) {
            return;
        }
        for rule in RULES.iter().filter(|rule| rule.token == token) {
            for surface in &rule.on {
                if *surface != "background" {
                    visit(*surface, order);
                }
            }
        }
        order.push(token);
    }
    let mut order = Vec::new();
    for rule in RULES.iter() {
        visit(rule.token, &mut order);
    }
    order
});

/// senpi `SystemThemeInput`.
#[derive(Debug, Clone, Default)]
pub struct SystemThemeInput {
    pub foreground: Option<RgbColor>,
    pub background: Option<RgbColor>,
    /// ANSI colors 0-15.
    pub palette: Option<Vec<RgbColor>>,
    /// Saturation multiplier from 0 (grayscale) to 1.
    pub saturation: Option<f64>,
    /// Appearance when the terminal did not report its background.
    pub appearance_hint: Option<ThemeAppearance>,
}

/// A generated token value: a hex color, an ANSI palette index, or the terminal default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SystemColor {
    Hex(String),
    Index(usize),
    Default,
}

/// senpi `SystemThemeColors`.
#[derive(Debug, Clone, PartialEq)]
pub struct SystemThemeColors {
    pub colors: BTreeMap<String, SystemColor>,
    /// Foreground tokens rendered faint (SGR 2), for terminals that did not report colors.
    pub dim: Vec<String>,
    pub appearance: Option<ThemeAppearance>,
}

fn color_of(rgb: RgbColor) -> Color {
    Color::Rgb {
        r: f64::from(rgb.r),
        g: f64::from(rgb.g),
        b: f64::from(rgb.b),
    }
}

/// OKLab lightness of an sRGB color, 0-1.
fn oklab_lightness(color: RgbColor) -> f64 {
    color_to_oklch(color_of(color)).l
}

/// WCAG 2 relative luminance.
pub fn relative_luminance(RgbColor { r, g, b }: RgbColor) -> f64 {
    let linear = |channel: u8| {
        let value = f64::from(channel) / 255.0;
        if value <= 0.04045 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b)
}

/// WCAG 2 contrast ratio, 1-21.
pub fn wcag_contrast(first: RgbColor, second: RgbColor) -> f64 {
    let a = relative_luminance(first);
    let b = relative_luminance(second);
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

const WHITE: RgbColor = RgbColor {
    r: 255,
    g: 255,
    b: 255,
};
const BLACK: RgbColor = RgbColor { r: 0, g: 0, b: 0 };

/// Whether a terminal is dark or light, from its reported colors: the direction of its own
/// foreground when text can be readable that way, otherwise dark when white text has more contrast
/// on the background than black text.
pub fn terminal_appearance(background: RgbColor, foreground: Option<RgbColor>) -> ThemeAppearance {
    let white_contrast = wcag_contrast(WHITE, background);
    let black_contrast = wcag_contrast(BLACK, background);
    if let Some(foreground) = foreground {
        let foreground_l = oklab_lightness(foreground);
        let background_l = oklab_lightness(background);
        if (foreground_l - background_l).abs() > 0.05 {
            let appearance = if foreground_l > background_l {
                ThemeAppearance::Dark
            } else {
                ThemeAppearance::Light
            };
            let best = if appearance == ThemeAppearance::Dark {
                white_contrast
            } else {
                black_contrast
            };
            if best >= TEXT_MINIMUM_WCAG_CONTRAST {
                return appearance;
            }
        }
    }
    if white_contrast >= black_contrast {
        ThemeAppearance::Dark
    } else {
        ThemeAppearance::Light
    }
}

fn hex_of(RgbColor { r, g, b }: RgbColor) -> String {
    format!("#{r:02x}{g:02x}{b:02x}")
}

/// Saturation weight at a lightness: a Gaussian (center 0.5, sigma 0.25), 0 at black and white.
fn bell_weight(lightness: f64) -> f64 {
    let gaussian = |x: f64| (-((x - 0.5).powi(2)) / (2.0 * 0.25_f64.powi(2))).exp();
    (gaussian(lightness) - gaussian(0.0)) / (1.0 - gaussian(0.0))
}

/// A family's saturation curve relative to its maximum.
fn saturation_curve(family: Family, lightness: f64) -> f64 {
    let floor = if family.max > 0.0 {
        family.min / family.max
    } else {
        1.0
    };
    floor + (1.0 - floor) * bell_weight(lightness)
}

/// The target lightness for a level on a surface, or `None` where the level cannot be reached.
fn level_target(level: Level, appearance: ThemeAppearance, surface_l: f64) -> Option<f64> {
    let curve = level_curve(level, appearance);
    if surface_l < curve.reachable.0 || surface_l > curve.reachable.1 {
        return None;
    }
    Some(
        curve
            .coefficients
            .iter()
            .enumerate()
            .map(|(power, coefficient)| coefficient * surface_l.powi(power as i32))
            .sum(),
    )
}

/// `okhslColor` from in-range channels (callers clamp), as rounded sRGB.
fn okhsl_rgb(h: f64, s: f64, l: f64) -> RgbColor {
    okhsl_color(h, s.clamp(0.0, 1.0), l.clamp(0.0, 1.0))
        .map(color_to_rgb)
        .unwrap_or(BLACK)
}

/// A terminal color's OKHSL channels and its OKLCH chroma.
#[derive(Clone, Copy)]
struct SourceColor {
    h: f64,
    s: f64,
    l: f64,
    chroma: f64,
}

fn source_of(color: RgbColor) -> SourceColor {
    let okhsl = color_to_okhsl(color_of(color));
    SourceColor {
        h: okhsl.h,
        s: okhsl.s,
        l: okhsl.l,
        chroma: color_to_oklch(color_of(color)).c,
    }
}

/// A source color's hue at another OKHSL lightness; saturation and chroma fall off toward black
/// and white along the family's curve and never rise above the source's.
fn anchored(source: SourceColor, family: Family, lightness: f64, saturation: f64) -> RgbColor {
    let anchor = saturation_curve(family, source.l);
    let falloff = if anchor > 0.0 {
        (saturation_curve(family, lightness) / anchor).min(1.0)
    } else {
        1.0
    };
    let color = okhsl_rgb(source.h, source.s * falloff * saturation, lightness);
    let cap = source.chroma * falloff * saturation;
    let oklch = color_to_oklch(color_of(color));
    if oklch.c <= cap {
        return color;
    }
    oklch_color(oklch.l, cap, source.h)
        .map(color_to_rgb)
        .unwrap_or(color)
}

/// Move a text color toward white or black until it reaches the WCAG minimum on every surface.
fn with_text_contrast(color: RgbColor, surfaces: &[RgbColor], lighter: bool) -> RgbColor {
    let meets = |candidate: RgbColor| {
        surfaces
            .iter()
            .all(|surface| wcag_contrast(candidate, *surface) >= TEXT_MINIMUM_WCAG_CONTRAST)
    };
    if meets(color) {
        return color;
    }
    let okhsl = color_to_okhsl(color_of(color));
    let at = |lightness: f64| okhsl_rgb(okhsl.h, okhsl.s, lightness);
    let extreme = if lighter { 1.0 } else { 0.0 };
    if !meets(at(extreme)) {
        return at(extreme);
    }
    let (mut low, mut high) = (okhsl.l, extreme);
    for _ in 0..20 {
        let middle = (low + high) / 2.0;
        if meets(at(middle)) {
            high = middle;
        } else {
            low = middle;
        }
    }
    at(high)
}

/// Colors for terminals that reported nothing: ANSI indices 0-15 and the default colors, which
/// the terminal renders with its own theme. Neutral tokens below body text are faint (SGR 2).
fn indexed_colors(saturation: f64, appearance: Option<ThemeAppearance>) -> SystemThemeColors {
    let mut colors = BTreeMap::new();
    let mut dim = Vec::new();
    for (token, family_name) in TOKEN_FAMILIES {
        if PANELS.contains(token) {
            colors.insert((*token).to_string(), SystemColor::Default);
            continue;
        }
        let neutral = *family_name == "neutral";
        let value = if !neutral && saturation > 0.0 {
            SystemColor::Index(token_slot(token).unwrap_or(family_of(family_name).slot))
        } else {
            SystemColor::Default
        };
        colors.insert((*token).to_string(), value);
        if neutral && !FOREGROUND_TOKENS.contains(token) {
            dim.push((*token).to_string());
        }
    }
    SystemThemeColors {
        colors,
        dim,
        appearance,
    }
}

fn extreme_of(values: &[f64], lighter: bool) -> f64 {
    if lighter {
        values.iter().copied().fold(f64::NEG_INFINITY, f64::max)
    } else {
        values.iter().copied().fold(f64::INFINITY, f64::min)
    }
}

/// Generate the system theme's colors from the terminal's reported colors.
pub fn generate_system_theme_colors(input: &SystemThemeInput) -> SystemThemeColors {
    let saturation = input.saturation.unwrap_or(1.0).clamp(0.0, 1.0);
    let Some(background) = input.background else {
        return indexed_colors(saturation, input.appearance_hint);
    };
    let foreground = input.foreground;
    let palette: Option<Vec<SourceColor>> = input
        .palette
        .as_ref()
        .filter(|palette| palette.len() == 16)
        .map(|palette| palette.iter().copied().map(source_of).collect());

    let appearance = terminal_appearance(background, foreground);
    let lighter = appearance == ThemeAppearance::Dark;
    let extreme = if lighter { 1.0 } else { 0.0 };
    let background_l = oklab_lightness(background);

    let paint = |token: &str, oklab_l: f64| -> RgbColor {
        let lightness = oklab_to_okhsl_lightness(oklab_l);
        let family = token_family(token);
        match &palette {
            None => okhsl_rgb(
                family.hue,
                (family.min + (family.max - family.min) * bell_weight(lightness)) * saturation,
                lightness,
            ),
            Some(palette) => anchored(
                palette[token_slot(token).unwrap_or(family.slot)],
                family,
                lightness,
                saturation,
            ),
        }
    };

    let target = |level: Level, surface_l: f64, t: f64| -> Option<f64> {
        let reached = level_target(level, appearance, surface_l);
        if reached.is_none() && t == 0.0 {
            return None;
        }
        let distance = reached.unwrap_or(extreme) - surface_l;
        let floor = level_target(readable_floor(appearance), appearance, surface_l)
            .unwrap_or(extreme)
            - surface_l;
        let compressed = if distance.abs() > floor.abs() {
            distance - (distance - floor) * t.min(1.0)
        } else {
            distance
        };
        Some(surface_l + compressed * (1.0 - (t - 1.0).max(0.0)))
    };

    let extreme_text = if lighter { WHITE } else { BLACK };
    let readable =
        |color: RgbColor| wcag_contrast(extreme_text, color) >= TEXT_MINIMUM_WCAG_CONTRAST;
    let limit_panel = |token: &str, l: f64| -> RgbColor {
        let color = paint(token, l);
        if readable(color) {
            return color;
        }
        let (mut low, mut high) = (background_l, l);
        for _ in 0..20 {
            let middle = (low + high) / 2.0;
            if readable(paint(token, middle)) {
                low = middle;
            } else {
                high = middle;
            }
        }
        paint(token, low)
    };

    let solve = |t: f64| -> Option<HashMap<&'static str, RgbColor>> {
        let mut colors: HashMap<&'static str, RgbColor> = HashMap::new();
        colors.insert("background", background);
        for token in SOLVE_ORDER.iter() {
            let mut targets = Vec::new();
            for rule in RULES.iter().filter(|rule| rule.token == *token) {
                for surface in &rule.on {
                    let surface_l =
                        oklab_lightness(colors.get(surface).copied().unwrap_or(background));
                    let value = target(rule.level, surface_l, t)?;
                    if !(0.0..=1.0).contains(&value) {
                        return None;
                    }
                    targets.push(value);
                }
            }
            let l = extreme_of(&targets, lighter);
            let color = if PANELS.contains(token) {
                limit_panel(token, l)
            } else {
                paint(token, l)
            };
            colors.insert(*token, color);
        }
        Some(colors)
    };

    let mut relaxation = 0.0;
    let mut colors = solve(0.0);
    if colors.is_none() {
        // Mid-gray backgrounds cannot fit every level: relax as little as possible.
        let (mut low, mut high) = (0.0, 2.0);
        colors = solve(high);
        for _ in 0..20 {
            let middle = (low + high) / 2.0;
            match solve(middle) {
                Some(attempt) => {
                    high = middle;
                    colors = Some(attempt);
                }
                None => low = middle,
            }
        }
        relaxation = high;
    }
    let solved = colors.unwrap_or_default();
    let surfaces_of = |token: &str| -> Vec<RgbColor> {
        RULES
            .iter()
            .filter(|rule| rule.token == token)
            .flat_map(|rule| {
                rule.on
                    .iter()
                    .map(|surface| solved.get(surface).copied().unwrap_or(background))
            })
            .collect()
    };

    let mut result: BTreeMap<String, SystemColor> = TOKEN_FAMILIES
        .iter()
        .map(|(token, _)| {
            let value = solved.get(token).map_or(SystemColor::Default, |color| {
                SystemColor::Hex(hex_of(*color))
            });
            ((*token).to_string(), value)
        })
        .collect();

    for token in FOREGROUND_TOKENS {
        let surfaces = surfaces_of(token);
        let mut text = solved.get(token).copied();
        if let Some(foreground) = foreground {
            let targets: Vec<Option<f64>> = surfaces
                .iter()
                .map(|surface| target(FOREGROUND_LEVEL, oklab_lightness(*surface), relaxation))
                .collect();
            if targets
                .iter()
                .all(|value| value.is_some_and(|value| (0.0..=1.0).contains(&value)))
            {
                let values: Vec<f64> = targets.into_iter().flatten().collect();
                let needed = extreme_of(&values, lighter);
                let foreground_l = oklab_lightness(foreground);
                if (lighter && foreground_l >= needed) || (!lighter && foreground_l <= needed) {
                    result.insert(token.to_string(), SystemColor::Default);
                    continue;
                }
                text = Some(anchored(
                    source_of(foreground),
                    NEUTRAL,
                    oklab_to_okhsl_lightness(needed),
                    saturation,
                ));
            }
        }
        if let Some(text) = text {
            result.insert(
                token.to_string(),
                SystemColor::Hex(hex_of(with_text_contrast(text, &surfaces, lighter))),
            );
        }
    }
    SystemThemeColors {
        colors: result,
        dim: Vec::new(),
        appearance: Some(appearance),
    }
}

#[cfg(test)]
mod tests {
    //! Port of the `generateSystemThemeColors` cases of senpi `test/system-theme.test.ts`. The
    //! two "system theme" cases exercise the theme registry (`getThemeByName("system")`), which
    //! this generator does not own.

    use maho_tui::colors::{ColorInput, parse_color};

    use super::*;

    fn hex(value: &str) -> RgbColor {
        color_to_rgb(parse_color(ColorInput::Text(value)).expect("valid hex"))
    }

    fn lightness(color: RgbColor) -> f64 {
        oklab_lightness(color)
    }

    fn palette(hexes: [&str; 16]) -> Option<Vec<RgbColor>> {
        Some(hexes.iter().map(|value| hex(value)).collect())
    }

    fn dracula() -> SystemThemeInput {
        SystemThemeInput {
            background: Some(hex("#282a36")),
            foreground: Some(hex("#f8f8f2")),
            palette: palette([
                "#21222c", "#ff5555", "#50fa7b", "#f1fa8c", "#bd93f9", "#ff79c6", "#8be9fd",
                "#f8f8f2", "#6272a4", "#ff6e6e", "#69ff94", "#ffffa5", "#d6acff", "#ff92df",
                "#a4ffff", "#ffffff",
            ]),
            ..SystemThemeInput::default()
        }
    }

    fn terminals() -> Vec<(&'static str, SystemThemeInput)> {
        vec![
            ("dracula", dracula()),
            (
                "solarizedLight",
                SystemThemeInput {
                    background: Some(hex("#fdf6e3")),
                    foreground: Some(hex("#657b83")),
                    ..SystemThemeInput::default()
                },
            ),
            (
                "backgroundOnly",
                SystemThemeInput {
                    background: Some(hex("#1e1e1e")),
                    ..SystemThemeInput::default()
                },
            ),
            (
                "midGray",
                SystemThemeInput {
                    background: Some(hex("#808080")),
                    foreground: Some(hex("#ffffff")),
                    ..SystemThemeInput::default()
                },
            ),
        ]
    }

    const TEST_PANELS: [&str; 5] = [
        "userMessageBg",
        "toolPendingBg",
        "toolSuccessBg",
        "toolErrorBg",
        "selectedBg",
    ];

    fn resolved(input: &SystemThemeInput, token: &str) -> RgbColor {
        match generate_system_theme_colors(input).colors.get(token) {
            Some(SystemColor::Hex(value)) => hex(value),
            _ if TEST_PANELS.contains(&token) => input.background.unwrap_or(BLACK),
            _ => input.foreground.unwrap_or(WHITE),
        }
    }

    #[test]
    fn keeps_body_text_readable_wcag_4_5_1_on_the_background_and_its_panels() {
        for (name, input) in terminals() {
            let text = resolved(&input, "text");
            let background = input.background.unwrap_or(BLACK);
            for surface in [background, resolved(&input, "selectedBg")] {
                assert!(wcag_contrast(text, surface) >= 4.5, "{name}");
            }
            assert!(
                wcag_contrast(
                    resolved(&input, "toolTitle"),
                    resolved(&input, "toolErrorBg")
                ) >= 4.5,
                "{name}"
            );
        }
    }

    #[test]
    fn orders_foreground_roles_by_contrast_and_keeps_panels_close_to_the_background() {
        for (name, input) in terminals() {
            let background = input.background.unwrap_or(BLACK);
            let offset = |token: &str| lightness(resolved(&input, token)) - lightness(background);
            if name != "midGray" {
                assert!(offset("text").abs() > offset("muted").abs(), "{name}");
                assert!(offset("muted").abs() > offset("dim").abs(), "{name}");
            }
            let lighter =
                generate_system_theme_colors(&input).appearance == Some(ThemeAppearance::Dark);
            for panel in TEST_PANELS {
                assert!(
                    wcag_contrast(resolved(&input, panel), background) < 2.0,
                    "{name} {panel}"
                );
                assert_eq!(offset(panel) > 0.0, lighter, "{name} {panel}");
            }
        }
    }

    #[test]
    fn uses_the_terminal_foreground_and_palette_hues() {
        let input = dracula();
        assert_eq!(
            generate_system_theme_colors(&input).colors.get("text"),
            Some(&SystemColor::Default)
        );
        let terminals = terminals();
        let solarized = &terminals[1].1;
        assert_ne!(
            generate_system_theme_colors(solarized).colors.get("text"),
            Some(&SystemColor::Default)
        );
        let hue = |color: RgbColor| color_to_oklch(color_of(color)).h;
        let red = input.palette.as_ref().map_or(BLACK, |palette| palette[1]);
        assert!((hue(resolved(&input, "error")) - hue(red)).abs() < 8.0);
    }

    #[test]
    fn keeps_pastel_palette_colors_pastel_at_other_lightnesses() {
        let frappe = SystemThemeInput {
            background: Some(hex("#303446")),
            foreground: Some(hex("#c6d0f5")),
            palette: palette([
                "#51576d", "#e78284", "#a6d189", "#e5c890", "#8caaee", "#f4b8e4", "#81c8be",
                "#b5bfe2", "#626880", "#e67172", "#8ec772", "#d9ba73", "#7b9ef0", "#f2a4db",
                "#5abfb5", "#a5adce",
            ]),
            ..SystemThemeInput::default()
        };
        let chroma = |color: RgbColor| color_to_oklch(color_of(color)).c;
        let pink = hex("#f4b8e4");
        let accent = resolved(&frappe, "accent");
        assert!(lightness(accent) < lightness(pink) - 0.05);
        assert!(chroma(accent) <= chroma(pink) * 1.03);
        for panel in ["userMessageBg", "customMessageBg"] {
            assert!(chroma(resolved(&frappe, panel)) <= 0.1, "{panel}");
        }
    }

    #[test]
    fn renders_grayscale_at_zero_saturation() {
        let input = SystemThemeInput {
            saturation: Some(0.0),
            ..dracula()
        };
        let colors = generate_system_theme_colors(&input).colors;
        let Some(SystemColor::Hex(error)) = colors.get("error") else {
            panic!("error is a hex color");
        };
        assert!(color_to_oklch(color_of(hex(error))).c < 0.005);
    }

    #[test]
    fn falls_back_to_palette_indices_and_faint_text_without_a_background() {
        let generated = generate_system_theme_colors(&SystemThemeInput {
            appearance_hint: Some(ThemeAppearance::Light),
            ..SystemThemeInput::default()
        });
        assert_eq!(generated.appearance, Some(ThemeAppearance::Light));
        assert_eq!(generated.colors.get("error"), Some(&SystemColor::Index(1)));
        assert_eq!(generated.colors.get("text"), Some(&SystemColor::Default));
        assert_eq!(
            generated.colors.get("userMessageBg"),
            Some(&SystemColor::Default)
        );
        assert!(generated.dim.iter().any(|token| token == "muted"));
        let gray = generate_system_theme_colors(&SystemThemeInput {
            saturation: Some(0.0),
            ..SystemThemeInput::default()
        });
        assert_eq!(gray.colors.get("error"), Some(&SystemColor::Default));
    }

    #[test]
    fn computes_wcag_contrast_and_appearance() {
        assert!((wcag_contrast(WHITE, BLACK) - 21.0).abs() < 1e-9);
        assert!((relative_luminance(WHITE) - 1.0).abs() < 1e-9);
        assert_eq!(
            terminal_appearance(hex("#1e1e1e"), None),
            ThemeAppearance::Dark
        );
        assert_eq!(
            terminal_appearance(hex("#fdf6e3"), None),
            ThemeAppearance::Light
        );
    }
}
