//! Port of theme.ts color and palette primitives.
use super::theme_json::{ColorValue, ThemeError, ThemeJson, validate_theme_json};
use super::theme_json::ThemeAppearance;
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
/// senpi `createTheme`'s default mode: `getCapabilities().trueColor ? "truecolor" : "256color"`
/// (the capabilities honour `COLORTERM` and the `PI_TRUE_COLOR` override).
pub fn detect_color_mode() -> ColorMode {
    if maho_tui::terminal_image::get_capabilities().true_color {
        ColorMode::Truecolor
    } else {
        ColorMode::Color256
    }
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
    pub appearance: Option<ThemeAppearance>,
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
        ColorValue::Text(text) if text.is_empty() || text.starts_with('#') || text.to_ascii_lowercase().starts_with("oklch(") || text.to_ascii_lowercase().starts_with("okhsl(") => Ok(value.clone()),
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
                    let text = if text.to_ascii_lowercase().starts_with("okhsl(") {
                        let [r, g, b] = parse_ok_color(&text).ok_or_else(|| ThemeError::Invalid(format!("Invalid color: {text}")))?;
                        format!("#{r:02x}{g:02x}{b:02x}")
                    } else { text };
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
                if t.to_ascii_lowercase().starts_with("oklch(") || t.to_ascii_lowercase().starts_with("okhsl(") {
                    parse_ok_color(t).ok_or_else(|| ThemeError::Invalid(format!("Invalid color: {t}")))?;
                } else { hex_to_rgb(t)?; }
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
            appearance: json.appearance,
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
            Some(ColorValue::Text(t)) if !t.is_empty() && (t.to_ascii_lowercase().starts_with("oklch(") || t.to_ascii_lowercase().starts_with("okhsl(")) => {
                let Some(rgb) = parse_ok_color(t) else { return String::new(); };
                match self.mode { ColorMode::Truecolor => format!("\x1b[{prefix};2;{};{};{}m",rgb[0],rgb[1],rgb[2]), ColorMode::Color256 => format!("\x1b[{prefix};5;{}m",rgb_to_256(rgb)) }
            }
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
                        ColorValue::Text(t) if t.to_ascii_lowercase().starts_with("oklch(") || t.to_ascii_lowercase().starts_with("okhsl(") => parse_ok_color(t).map_or_else(|| t.clone(), |rgb| format!("#{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2])),
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
fn parse_ok_color(text: &str) -> Option<[u8; 3]> {
    let lower = text.to_ascii_lowercase();
    let is_okhsl = lower.starts_with("okhsl(");
    let is_oklch = lower.starts_with("oklch(");
    if !is_okhsl && !is_oklch { return None; }
    let inside = text.split_once('(')?.1.strip_suffix(')')?;
    let fields: Vec<&str> = inside.split_whitespace().collect();
    if fields.len() != 3 { return None; }
    let number = |field: &str| -> Option<f64> {
        let value = field.trim_end_matches('%').trim_end_matches("deg").parse::<f64>().ok()?;
        value.is_finite().then_some(value)
    };
    let lightness_field = fields[if is_okhsl { 2 } else { 0 }];
    let lightness = number(lightness_field)? / if lightness_field.ends_with('%') { 100.0 } else { 1.0 };
    let second = number(fields[1])? / if fields[1].ends_with('%') { 100.0 } else { 1.0 };
    let hue = number(fields[if is_okhsl { 0 } else { 2 }])?;
    if !(0.0..=1.0).contains(&lightness) || second < 0.0 || (is_okhsl && second > 1.0) { return None; }
    let rgb = if is_okhsl {
        okhsl_to_rgb(hue, second, lightness)
    } else {
        oklch_to_rgb(lightness, second, hue)
    };
    Some(rgb)
}

// Oklab and OKHSL conversion math ported from packages/tui/src/oklab.ts.
// Copyright (c) 2021 Bjorn Ottosson, used under the MIT license.
// Permission is hereby granted, free of charge, to any person obtaining a copy
// of this software and associated documentation files (the "Software"), to deal
// in the Software without restriction, including without limitation the rights
// to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
// copies of the Software, and to permit persons to whom the Software is
// furnished to do so, subject to the following conditions:
// The above copyright notice and this permission notice shall be included in all
// copies or substantial portions of the Software.
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
// IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
// OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
// SOFTWARE.
type Vector = [f64; 3];
type Matrix = [[f64; 3]; 3];
const LAB_TO_LMS: Matrix = [[1.0,0.3963377773761749,0.2158037573099136],[1.0,-0.1055613458156586,-0.0638541728258133],[1.0,-0.0894841775298119,-1.2914855480194092]];
const LMS_TO_LINEAR_SRGB: Matrix = [[4.076_741_636_075_958,-3.307_711_539_258_063,0.2309699031821043],[-1.2684379732850315,2.609_757_349_287_688,-0.341319376002657],[-0.0041960761386756,-0.7034186179359362,1.7076146940746117]];
fn multiply(matrix: Matrix, [x,y,z]: Vector) -> Vector { matrix.map(|row| row[0]*x+row[1]*y+row[2]*z) }
fn oklab_to_linear_srgb(lab: Vector) -> Vector { multiply(LMS_TO_LINEAR_SRGB, multiply(LAB_TO_LMS, lab).map(|v| v.powi(3))) }
fn linear_to_rgb(linear: Vector) -> [u8; 3] {
    linear.map(|v| { let encoded = if v > 0.0031308 { 1.055*v.powf(1.0/2.4)-0.055 } else { 12.92*v }; (encoded.clamp(0.0,1.0)*255.0).round() as u8 })
}
const SATURATION_FIT: [([f64;2],[f64;5]);3] = [([-1.8817031,-0.80936501],[1.19086277,1.76576728,0.59662641,0.75515197,0.56771245]),([1.8144408,-1.19445267],[0.73956515,-0.45954404,0.08285427,0.12541073,-0.14503204]),([0.13110758,1.81333971],[1.35733652,-0.00915799,-1.1513021,-0.50559606,0.00692167])];
fn lms_slopes(a:f64,b:f64)->Vector { [a*LAB_TO_LMS[0][1]+b*LAB_TO_LMS[0][2],a*LAB_TO_LMS[1][1]+b*LAB_TO_LMS[1][2],a*LAB_TO_LMS[2][1]+b*LAB_TO_LMS[2][2]] }
fn max_saturation(a:f64,b:f64)->f64 {
    let channel=SATURATION_FIT.iter().position(|(xy,_)| xy[0]*a+xy[1]*b>1.0).unwrap_or(2);
    let [k0,k1,k2,k3,k4]=SATURATION_FIT[channel].1;
    let weights=LMS_TO_LINEAR_SRGB[channel]; let saturation=k0+k1*a+k2*b+k3*a*a+k4*a*b; let slopes=lms_slopes(a,b);
    let base=slopes.map(|k|1.0+saturation*k);
    let dot=|v:Vector| weights[0]*v[0]+weights[1]*v[1]+weights[2]*v[2];
    let f=dot(base.map(|v|v.powi(3))); let f1=dot(std::array::from_fn(|i|3.0*slopes[i]*base[i].powi(2))); let f2=dot(std::array::from_fn(|i|6.0*slopes[i].powi(2)*base[i]));
    saturation-f*f1/(f1*f1-0.5*f*f2)
}
fn cusp(a:f64,b:f64)->(f64,f64) { let s=max_saturation(a,b); let l=oklab_to_linear_srgb([1.0,s*a,s*b]).into_iter().fold(f64::NEG_INFINITY,f64::max).cbrt().recip(); (l,l*s) }
fn max_chroma(a:f64,b:f64,l:f64,peak:(f64,f64))->f64 {
    let (cl,cc)=peak; if l<=cl { return cc*l/cl; }
    let t=cc*(l-1.0)/(cl-1.0); let slopes=lms_slopes(a,b); let lms=slopes.map(|k|l+t*k); let cubes=lms.map(|v|v.powi(3));
    let first=std::array::from_fn(|i|3.0*slopes[i]*lms[i].powi(2)); let second=std::array::from_fn(|i|6.0*slopes[i].powi(2)*lms[i]);
    let steps=LMS_TO_LINEAR_SRGB.map(|row| { let dot=|v:Vector|row[0]*v[0]+row[1]*v[1]+row[2]*v[2]; let f=dot(cubes)-1.0; let f1=dot(first); let f2=dot(second); let u=f1/(f1*f1-0.5*f*f2); if u>=0.0 {-f*u} else {f64::MAX} });
    t+steps.into_iter().fold(f64::INFINITY,f64::min)
}
fn chroma_stops(l:f64,a:f64,b:f64)->(f64,f64,f64) {
    let peak=cusp(a,b); let cmax=max_chroma(a,b,l,peak); let k=cmax/(l*(peak.1/peak.0)).min((1.0-l)*(peak.1/(1.0-peak.0)));
    let mids=0.11516993+1.0/(7.4477897+4.1590124*b+a*(-2.19557347+1.75198401*b+a*(-2.13704948-10.02301043*b+a*(-4.24894561+5.38770819*b+4.69891013*a))));
    let midt=0.11239642+1.0/(1.6132032-0.68124379*b+a*(0.40370612+0.90148123*b+a*(-0.27087943+0.6122399*b+a*(0.00299215-0.45399568*b-0.14661872*a))));
    let cmid=0.9*k*(1.0/(1.0/(l*mids).powi(4)+1.0/((1.0-l)*midt).powi(4))).sqrt().sqrt();
    let c0=(1.0/(1.0/(l*0.4).powi(2)+1.0/((1.0-l)*0.8).powi(2))).sqrt(); (c0,cmid,cmax)
}
fn okhsl_to_rgb(hue:f64,s:f64,l:f64)->[u8;3] {
    const K1:f64=0.206; const K2:f64=0.03; let k3=(1.0+K1)/(1.0+K2); let ll=(l*l+K1*l)/(k3*(l+K2));
    if ll<=0.0 || ll>=1.0 || s<=0.0 { return linear_to_rgb(oklab_to_linear_srgb([ll,0.0,0.0])); }
    let angle=hue.rem_euclid(360.0).to_radians(); let (a,b)=(angle.cos(),angle.sin()); let (c0,cmid,cmax)=chroma_stops(ll,a,b);
    let chroma=if s<0.8 { let t=1.25*s; let k1=0.8*c0; t*k1/(1.0-(1.0-k1/cmid)*t) } else { let t=5.0*(s-0.8); let k1=0.2*cmid.powi(2)*1.25_f64.powi(2)/c0; cmid+t*k1/(1.0-(1.0-k1/(cmax-cmid))*t) };
    linear_to_rgb(oklab_to_linear_srgb([ll,chroma*a,chroma*b]))
}
fn oklch_to_rgb(l:f64,c:f64,h:f64)->[u8;3] {
    let angle=h.to_radians(); let at=|chroma:f64|oklab_to_linear_srgb([l,chroma*angle.cos(),chroma*angle.sin()]);
    let direct=at(c); if direct.iter().all(|v|*v>=-1e-7&&*v<=1.0+1e-7) { return linear_to_rgb(direct); }
    let mut low=0.0; let mut high=c; let mut linear=at(0.0);
    for _ in 0..20 { let chroma=(low+high)/2.0; let candidate=at(chroma); if candidate.iter().all(|v|*v>=-1e-7&&*v<=1.0+1e-7) { low=chroma; linear=candidate; } else { high=chroma; } }
    linear_to_rgb(linear)
}
fn style(t: &str, open: u8, close: u8) -> String {
    if t.is_empty() {
        return String::new();
    }
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
/// senpi `detectTerminalBackgroundTheme`'s COLORFGBG branch, delegated to the faithful
/// `getColorFgBgBackgroundIndex` port in [`super::detection`] (a `parseInt` prefix such as `15x`
/// is accepted).
pub fn detect_terminal_background_from_env(colorfgbg: Option<&str>) -> TerminalThemeDetection {
    let detection = super::detection::detect_terminal_background_from_env(colorfgbg);
    TerminalThemeDetection {
        theme: detection.theme,
        source: detection.source,
        detail: detection.detail,
        high_confidence: detection.confidence == "high",
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
#[cfg(test)]
mod oklch_tests {
    use super::*;
    #[test]
    fn parses_oklch_to_rgb() {
        let rgb = parse_ok_color("oklch(0.5 0.1 30)").expect("valid OKLCH");
        assert!(rgb[0] > rgb[1]);
        assert!(rgb[0] > rgb[2]);
    }
}
