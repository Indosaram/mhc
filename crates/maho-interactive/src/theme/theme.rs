//! Port of theme.ts color and palette primitives.
use super::theme_json::{ColorValue, ThemeError, ThemeJson, validate_theme_json};
use std::collections::{BTreeMap, BTreeSet};
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ThemeColor {
    #[serde(rename = "accent")]
    Accent,
    #[serde(rename = "border")]
    Border,
    #[serde(rename = "borderAccent")]
    BorderAccent,
    #[serde(rename = "borderMuted")]
    BorderMuted,
    #[serde(rename = "success")]
    Success,
    #[serde(rename = "error")]
    Error,
    #[serde(rename = "warning")]
    Warning,
    #[serde(rename = "muted")]
    Muted,
    #[serde(rename = "dim")]
    Dim,
    #[serde(rename = "text")]
    Text,
    #[serde(rename = "thinkingText")]
    ThinkingText,
    #[serde(rename = "scrollbarTrack")]
    ScrollbarTrack,
    #[serde(rename = "scrollbarThumb")]
    ScrollbarThumb,
    #[serde(rename = "searchMatchText")]
    SearchMatchText,
    #[serde(rename = "userMessageText")]
    UserMessageText,
    #[serde(rename = "customMessageText")]
    CustomMessageText,
    #[serde(rename = "customMessageLabel")]
    CustomMessageLabel,
    #[serde(rename = "toolTitle")]
    ToolTitle,
    #[serde(rename = "toolOutput")]
    ToolOutput,
    #[serde(rename = "mdHeading")]
    MdHeading,
    #[serde(rename = "mdLink")]
    MdLink,
    #[serde(rename = "mdLinkUrl")]
    MdLinkUrl,
    #[serde(rename = "skillMention")]
    SkillMention,
    #[serde(rename = "mdCode")]
    MdCode,
    #[serde(rename = "mdCodeBlock")]
    MdCodeBlock,
    #[serde(rename = "mdCodeBlockBorder")]
    MdCodeBlockBorder,
    #[serde(rename = "mdQuote")]
    MdQuote,
    #[serde(rename = "mdQuoteBorder")]
    MdQuoteBorder,
    #[serde(rename = "mdHr")]
    MdHr,
    #[serde(rename = "mdListBullet")]
    MdListBullet,
    #[serde(rename = "toolDiffAdded")]
    ToolDiffAdded,
    #[serde(rename = "toolDiffRemoved")]
    ToolDiffRemoved,
    #[serde(rename = "toolDiffContext")]
    ToolDiffContext,
    #[serde(rename = "syntaxComment")]
    SyntaxComment,
    #[serde(rename = "syntaxKeyword")]
    SyntaxKeyword,
    #[serde(rename = "syntaxFunction")]
    SyntaxFunction,
    #[serde(rename = "syntaxVariable")]
    SyntaxVariable,
    #[serde(rename = "syntaxString")]
    SyntaxString,
    #[serde(rename = "syntaxNumber")]
    SyntaxNumber,
    #[serde(rename = "syntaxType")]
    SyntaxType,
    #[serde(rename = "syntaxOperator")]
    SyntaxOperator,
    #[serde(rename = "syntaxPunctuation")]
    SyntaxPunctuation,
    #[serde(rename = "thinkingOff")]
    ThinkingOff,
    #[serde(rename = "thinkingMinimal")]
    ThinkingMinimal,
    #[serde(rename = "thinkingLow")]
    ThinkingLow,
    #[serde(rename = "thinkingMedium")]
    ThinkingMedium,
    #[serde(rename = "thinkingHigh")]
    ThinkingHigh,
    #[serde(rename = "thinkingXhigh")]
    ThinkingXhigh,
    #[serde(rename = "thinkingMax")]
    ThinkingMax,
    #[serde(rename = "bashMode")]
    BashMode,
}
impl ThemeColor {
    pub const ALL: &'static [Self] = &[
        Self::Accent,
        Self::Border,
        Self::BorderAccent,
        Self::BorderMuted,
        Self::Success,
        Self::Error,
        Self::Warning,
        Self::Muted,
        Self::Dim,
        Self::Text,
        Self::ThinkingText,
        Self::ScrollbarTrack,
        Self::ScrollbarThumb,
        Self::SearchMatchText,
        Self::UserMessageText,
        Self::CustomMessageText,
        Self::CustomMessageLabel,
        Self::ToolTitle,
        Self::ToolOutput,
        Self::MdHeading,
        Self::MdLink,
        Self::MdLinkUrl,
        Self::SkillMention,
        Self::MdCode,
        Self::MdCodeBlock,
        Self::MdCodeBlockBorder,
        Self::MdQuote,
        Self::MdQuoteBorder,
        Self::MdHr,
        Self::MdListBullet,
        Self::ToolDiffAdded,
        Self::ToolDiffRemoved,
        Self::ToolDiffContext,
        Self::SyntaxComment,
        Self::SyntaxKeyword,
        Self::SyntaxFunction,
        Self::SyntaxVariable,
        Self::SyntaxString,
        Self::SyntaxNumber,
        Self::SyntaxType,
        Self::SyntaxOperator,
        Self::SyntaxPunctuation,
        Self::ThinkingOff,
        Self::ThinkingMinimal,
        Self::ThinkingLow,
        Self::ThinkingMedium,
        Self::ThinkingHigh,
        Self::ThinkingXhigh,
        Self::ThinkingMax,
        Self::BashMode,
    ];
    pub const fn key(self) -> &'static str {
        match self {
            Self::Accent => "accent",
            Self::Border => "border",
            Self::BorderAccent => "borderAccent",
            Self::BorderMuted => "borderMuted",
            Self::Success => "success",
            Self::Error => "error",
            Self::Warning => "warning",
            Self::Muted => "muted",
            Self::Dim => "dim",
            Self::Text => "text",
            Self::ThinkingText => "thinkingText",
            Self::ScrollbarTrack => "scrollbarTrack",
            Self::ScrollbarThumb => "scrollbarThumb",
            Self::SearchMatchText => "searchMatchText",
            Self::UserMessageText => "userMessageText",
            Self::CustomMessageText => "customMessageText",
            Self::CustomMessageLabel => "customMessageLabel",
            Self::ToolTitle => "toolTitle",
            Self::ToolOutput => "toolOutput",
            Self::MdHeading => "mdHeading",
            Self::MdLink => "mdLink",
            Self::MdLinkUrl => "mdLinkUrl",
            Self::SkillMention => "skillMention",
            Self::MdCode => "mdCode",
            Self::MdCodeBlock => "mdCodeBlock",
            Self::MdCodeBlockBorder => "mdCodeBlockBorder",
            Self::MdQuote => "mdQuote",
            Self::MdQuoteBorder => "mdQuoteBorder",
            Self::MdHr => "mdHr",
            Self::MdListBullet => "mdListBullet",
            Self::ToolDiffAdded => "toolDiffAdded",
            Self::ToolDiffRemoved => "toolDiffRemoved",
            Self::ToolDiffContext => "toolDiffContext",
            Self::SyntaxComment => "syntaxComment",
            Self::SyntaxKeyword => "syntaxKeyword",
            Self::SyntaxFunction => "syntaxFunction",
            Self::SyntaxVariable => "syntaxVariable",
            Self::SyntaxString => "syntaxString",
            Self::SyntaxNumber => "syntaxNumber",
            Self::SyntaxType => "syntaxType",
            Self::SyntaxOperator => "syntaxOperator",
            Self::SyntaxPunctuation => "syntaxPunctuation",
            Self::ThinkingOff => "thinkingOff",
            Self::ThinkingMinimal => "thinkingMinimal",
            Self::ThinkingLow => "thinkingLow",
            Self::ThinkingMedium => "thinkingMedium",
            Self::ThinkingHigh => "thinkingHigh",
            Self::ThinkingXhigh => "thinkingXhigh",
            Self::ThinkingMax => "thinkingMax",
            Self::BashMode => "bashMode",
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ThemeBg {
    #[serde(rename = "selectedBg")]
    SelectedBg,
    #[serde(rename = "searchMatchBg")]
    SearchMatchBg,
    #[serde(rename = "userMessageBg")]
    UserMessageBg,
    #[serde(rename = "customMessageBg")]
    CustomMessageBg,
    #[serde(rename = "toolPendingBg")]
    ToolPendingBg,
    #[serde(rename = "toolSuccessBg")]
    ToolSuccessBg,
    #[serde(rename = "toolErrorBg")]
    ToolErrorBg,
}
impl ThemeBg {
    pub const ALL: &'static [Self] = &[
        Self::SelectedBg,
        Self::SearchMatchBg,
        Self::UserMessageBg,
        Self::CustomMessageBg,
        Self::ToolPendingBg,
        Self::ToolSuccessBg,
        Self::ToolErrorBg,
    ];
    pub const fn key(self) -> &'static str {
        match self {
            Self::SelectedBg => "selectedBg",
            Self::SearchMatchBg => "searchMatchBg",
            Self::UserMessageBg => "userMessageBg",
            Self::CustomMessageBg => "customMessageBg",
            Self::ToolPendingBg => "toolPendingBg",
            Self::ToolSuccessBg => "toolSuccessBg",
            Self::ToolErrorBg => "toolErrorBg",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorMode {
    Truecolor,
    Color256,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TerminalTheme {
    Dark,
    Light,
}
impl TerminalTheme {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Dark => "dark",
            Self::Light => "light",
        }
    }
}
#[derive(Debug, Clone)]
pub struct Theme {
    pub name: String,
    pub source_path: Option<std::path::PathBuf>,
    mode: ColorMode,
    colors: BTreeMap<String, ColorValue>,
    export_colors: BTreeMap<String, String>,
}
fn resolve(
    value: &ColorValue,
    vars: &BTreeMap<String, ColorValue>,
    visited: &mut BTreeSet<String>,
) -> Result<ColorValue, ThemeError> {
    match value {
        ColorValue::Index(_) => Ok(value.clone()),
        ColorValue::Text(text) if text.is_empty() || text.starts_with('#') => Ok(value.clone()),
        ColorValue::Text(text) => {
            if !visited.insert(text.clone()) {
                return Err(ThemeError::Invalid(format!(
                    "Circular variable reference detected: {text}"
                )));
            }
            resolve(
                vars.get(text).ok_or_else(|| {
                    ThemeError::Invalid(format!("Variable reference not found: {text}"))
                })?,
                vars,
                visited,
            )
        }
    }
}
pub fn hex_to_rgb(hex: &str) -> Result<[u8; 3], ThemeError> {
    let value = hex.strip_prefix('#').unwrap_or(hex);
    if value.len() != 6 || !value.is_ascii() {
        return Err(ThemeError::Invalid(format!("Invalid hex color: {hex}")));
    }
    let mut rgb = [0; 3];
    for (i, c) in rgb.iter_mut().enumerate() {
        *c = u8::from_str_radix(&value[i * 2..i * 2 + 2], 16)
            .map_err(|_| ThemeError::Invalid(format!("Invalid hex color: {hex}")))?;
    }
    Ok(rgb)
}
pub fn rgb_to_256([r, g, b]: [u8; 3]) -> u8 {
    let cube = [0u8, 95, 135, 175, 215, 255];
    let nearest = |v: u8| {
        cube.iter()
            .enumerate()
            .min_by_key(|(_, c)| i32::from(v).abs_diff(i32::from(**c)))
            .map_or(0, |(i, _)| i)
    };
    let ri = nearest(r);
    let gi = nearest(g);
    let bi = nearest(b);
    let distance = |a: [u8; 3], c: [u8; 3]| {
        a.into_iter()
            .zip(c)
            .zip([0.299, 0.587, 0.114])
            .map(|((x, y), w)| f64::from(i32::from(x) - i32::from(y)).powi(2) * w)
            .sum::<f64>()
    };
    let gray = (0.299 * f64::from(r) + 0.587 * f64::from(g) + 0.114 * f64::from(b)).round();
    let gray_i = (0u8..24)
        .min_by(|a, b| {
            (f64::from(8 + *a * 10) - gray)
                .abs()
                .total_cmp(&(f64::from(8 + *b * 10) - gray).abs())
        })
        .unwrap_or(0);
    let gray_v = 8 + gray_i * 10;
    if r.max(g).max(b) - r.min(g).min(b) < 10
        && distance([r, g, b], [gray_v; 3]) < distance([r, g, b], [cube[ri], cube[gi], cube[bi]])
    {
        232 + gray_i
    } else {
        u8::try_from(16 + 36 * ri + 6 * gi + bi).unwrap_or(16)
    }
}
pub fn ansi256_to_hex(index: u8) -> String {
    let basic = [
        "#000000", "#800000", "#008000", "#808000", "#000080", "#800080", "#008080", "#c0c0c0",
        "#808080", "#ff0000", "#00ff00", "#ffff00", "#0000ff", "#ff00ff", "#00ffff", "#ffffff",
    ];
    if index < 16 {
        return basic[usize::from(index)].into();
    }
    let [r, g, b] = if index < 232 {
        let i = index - 16;
        let channel = |x: u8| if x == 0 { 0 } else { 55 + x * 40 };
        [channel(i / 36), channel(i % 36 / 6), channel(i % 6)]
    } else {
        [8 + (index - 232) * 10; 3]
    };
    format!("#{r:02x}{g:02x}{b:02x}")
}
impl Theme {
    pub fn from_json(json: ThemeJson, mode: ColorMode) -> Result<Self, ThemeError> {
        let mut export_colors = BTreeMap::new();
        for (key, value) in &json.export_colors {
            match resolve(value, &json.vars, &mut BTreeSet::new())? {
                ColorValue::Index(index) => {
                    export_colors.insert(key.clone(), ansi256_to_hex(index));
                }
                ColorValue::Text(text) if !text.is_empty() => {
                    export_colors.insert(key.clone(), text);
                }
                ColorValue::Text(_) => {}
            }
        }
        let mut colors = BTreeMap::new();
        for (key, value) in &json.colors {
            let value = resolve(value, &json.vars, &mut BTreeSet::new())?;
            if let ColorValue::Text(t) = &value
                && !t.is_empty()
            {
                hex_to_rgb(t)?;
            }
            colors.insert(key.clone(), value);
        }
        for (k, f) in [
            ("scrollbarTrack", "muted"),
            ("scrollbarThumb", "text"),
            ("thinkingMax", "thinkingXhigh"),
            ("searchMatchText", "text"),
            ("skillMention", "mdLink"),
            ("searchMatchBg", "selectedBg"),
        ] {
            if !colors.contains_key(k)
                && let Some(v) = colors.get(f)
            {
                colors.insert(k.into(), v.clone());
            }
        }
        Ok(Self {
            name: json.name,
            source_path: None,
            mode,
            colors,
            export_colors,
        })
    }
    pub fn builtin(name: &str, mode: ColorMode) -> Result<Self, ThemeError> {
        let text = match name {
            "dark" => include_str!("dark.json"),
            "light" => include_str!("light.json"),
            "grok-day" => include_str!("grok-day.json"),
            "grok-night" => include_str!("grok-night.json"),
            _ => return Err(ThemeError::Invalid(format!("Theme not found: {name}"))),
        };
        Self::from_json(
            validate_theme_json(name, serde_json::from_str(text)?)?,
            mode,
        )
    }
    pub fn load_from_path(path: &std::path::Path, mode: ColorMode) -> Result<Self, ThemeError> {
        let mut t = Self::from_json(
            validate_theme_json(
                &path.display().to_string(),
                serde_json::from_str(&std::fs::read_to_string(path)?)?,
            )?,
            mode,
        )?;
        t.source_path = Some(path.to_path_buf());
        Ok(t)
    }
    fn ansi(&self, key: &str, bg: bool) -> String {
        let prefix = if bg { 48 } else { 38 };
        match self.colors.get(key) {
            Some(ColorValue::Index(i)) => format!("\x1b[{prefix};5;{i}m"),
            Some(ColorValue::Text(t)) if !t.is_empty() => match hex_to_rgb(t) {
                Ok(rgb) => match self.mode {
                    ColorMode::Truecolor => {
                        format!("\x1b[{prefix};2;{};{};{}m", rgb[0], rgb[1], rgb[2])
                    }
                    ColorMode::Color256 => format!("\x1b[{prefix};5;{}m", rgb_to_256(rgb)),
                },
                Err(_) => String::new(),
            },
            _ => format!("\x1b[{}m", if bg { 49 } else { 39 }),
        }
    }
    pub fn fg(&self, color: ThemeColor, text: &str) -> String {
        format!("{}{text}\x1b[39m", self.get_fg_ansi(color))
    }
    pub fn bg(&self, color: ThemeBg, text: &str) -> String {
        format!("{}{text}\x1b[49m", self.get_bg_ansi(color))
    }
    pub fn get_fg_ansi(&self, c: ThemeColor) -> String {
        self.ansi(c.key(), false)
    }
    pub fn get_bg_ansi(&self, c: ThemeBg) -> String {
        self.ansi(c.key(), true)
    }
    pub const fn get_color_mode(&self) -> ColorMode {
        self.mode
    }
    pub fn bold(&self, t: &str) -> String {
        style(t, 1, 22)
    }
    pub fn italic(&self, t: &str) -> String {
        style(t, 3, 23)
    }
    pub fn underline(&self, t: &str) -> String {
        style(t, 4, 24)
    }
    pub fn inverse(&self, t: &str) -> String {
        style(t, 7, 27)
    }
    pub fn strikethrough(&self, t: &str) -> String {
        style(t, 9, 29)
    }
    pub fn resolved_colors(&self) -> BTreeMap<String, String> {
        self.colors
            .iter()
            .map(|(k, v)| {
                (
                    k.clone(),
                    match v {
                        ColorValue::Index(i) => ansi256_to_hex(*i),
                        ColorValue::Text(t) if t.is_empty() => {
                            if self.name == "light" {
                                "#000000".into()
                            } else {
                                "#e5e5e7".into()
                            }
                        }
                        ColorValue::Text(t) => t.clone(),
                    },
                )
            })
            .collect()
    }
    pub fn export_colors(&self) -> &BTreeMap<String, String> {
        &self.export_colors
    }
    pub fn thinking_border_color(&self, level: &str, text: &str) -> String {
        let color = match level {
            "minimal" => ThemeColor::ThinkingMinimal,
            "low" => ThemeColor::ThinkingLow,
            "medium" => ThemeColor::ThinkingMedium,
            "high" => ThemeColor::ThinkingHigh,
            "xhigh" => ThemeColor::ThinkingXhigh,
            "max" => ThemeColor::ThinkingMax,
            _ => ThemeColor::ThinkingOff,
        };
        self.fg(color, text)
    }
    pub fn bash_mode_border_color(&self, text: &str) -> String {
        self.fg(ThemeColor::BashMode, text)
    }
}
fn style(t: &str, open: u8, close: u8) -> String {
    format!(
        "\x1b[{open}m{}\x1b[{close}m",
        t.replace(&format!("\x1b[{close}m"), &format!("\x1b[{open}m"))
    )
}
pub fn parse_auto_theme_setting(setting: Option<&str>) -> Option<(&str, &str)> {
    let (s1, s2) = setting?.split_once('/')?;
    let (a, b) = (s1.trim(), s2.trim());
    if a.is_empty() || b.is_empty() || b.contains('/') {
        None
    } else {
        Some((a, b))
    }
}
pub fn resolve_theme_setting(setting: Option<&str>, terminal: TerminalTheme) -> Option<&str> {
    match parse_auto_theme_setting(setting) {
        Some((light, dark)) => Some(match terminal {
            TerminalTheme::Light => light,
            TerminalTheme::Dark => dark,
        }),
        None => setting.filter(|s| !s.contains('/')),
    }
}
pub fn get_theme_for_rgb_color([r, g, b]: [u8; 3]) -> TerminalTheme {
    let linear = |x: u8| {
        let x = f64::from(x) / 255.;
        if x <= 0.03928 {
            x / 12.92
        } else {
            ((x + 0.055) / 1.055).powf(2.4)
        }
    };
    if 0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b) >= 0.5 {
        TerminalTheme::Light
    } else {
        TerminalTheme::Dark
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalThemeDetection {
    pub theme: TerminalTheme,
    pub source: &'static str,
    pub detail: String,
    pub high_confidence: bool,
}
pub fn detect_terminal_background_from_env(colorfgbg: Option<&str>) -> TerminalThemeDetection {
    if let Some(index) = colorfgbg.and_then(|s| {
        s.split(';')
            .rev()
            .find_map(|field| field.trim().parse::<u8>().ok())
    }) {
        let rgb = hex_to_rgb(&ansi256_to_hex(index)).unwrap_or([0; 3]);
        TerminalThemeDetection {
            theme: get_theme_for_rgb_color(rgb),
            source: "COLORFGBG",
            detail: format!("background color index {index}"),
            high_confidence: true,
        }
    } else {
        TerminalThemeDetection {
            theme: TerminalTheme::Dark,
            source: "fallback",
            detail: "no terminal background hint found".into(),
            high_confidence: false,
        }
    }
}
pub fn detect_terminal_background_theme(
    rgb: Option<[u8; 3]>,
    colorfgbg: Option<&str>,
) -> TerminalThemeDetection {
    if let Some(rgb) = rgb {
        TerminalThemeDetection {
            theme: get_theme_for_rgb_color(rgb),
            source: "terminal background",
            detail: format!("OSC 11 background rgb({}, {}, {})", rgb[0], rgb[1], rgb[2]),
            high_confidence: true,
        }
    } else {
        detect_terminal_background_from_env(colorfgbg)
    }
}
pub fn get_theme_export_colors(json: &ThemeJson) -> Result<BTreeMap<String, String>, ThemeError> {
    let mut colors = BTreeMap::new();
    for (key, value) in &json.export_colors {
        match resolve(value, &json.vars, &mut BTreeSet::new())? {
            ColorValue::Index(i) => {
                colors.insert(key.clone(), ansi256_to_hex(i));
            }
            ColorValue::Text(t) if !t.is_empty() => {
                colors.insert(key.clone(), t);
            }
            ColorValue::Text(_) => {}
        }
    }
    Ok(colors)
}
impl Theme {
    pub fn get_thinking_border_color(
        &self,
        level: maho_ai::types::ModelThinkingLevel,
        text: &str,
    ) -> String {
        use maho_ai::types::ModelThinkingLevel;
        self.fg(
            match level {
                ModelThinkingLevel::Off => ThemeColor::ThinkingOff,
                ModelThinkingLevel::Minimal => ThemeColor::ThinkingMinimal,
                ModelThinkingLevel::Low => ThemeColor::ThinkingLow,
                ModelThinkingLevel::Medium => ThemeColor::ThinkingMedium,
                ModelThinkingLevel::High => ThemeColor::ThinkingHigh,
                ModelThinkingLevel::Xhigh => ThemeColor::ThinkingXhigh,
                ModelThinkingLevel::Max => ThemeColor::ThinkingMax,
            },
            text,
        )
    }
    pub fn get_bash_mode_border_color(&self, text: &str) -> String {
        self.fg(ThemeColor::BashMode, text)
    }
}
