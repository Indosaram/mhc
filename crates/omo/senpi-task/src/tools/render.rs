//! Host rendering and tool-result seams shared by every tool surface.
//!
//! TypeScript imports these from `@code-yeongyu/senpi` (`Theme`, `ThemeColor`, `AgentToolResult`,
//! `ToolRenderResultOptions`) and `@earendil-works/pi-tui` (`truncateToWidth`). senpi-task never
//! links the host, so the pieces it consumes are modelled here; the harness adapts them in todo 44.

use serde::Serialize;
use serde_json::Value;

use crate::renderer_text::renderer_visible_width;

/// senpi `ThemeColor` (`modes/interactive/theme/theme.ts`), serialized with its TS spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ThemeColor {
    Accent,
    Border,
    BorderAccent,
    BorderMuted,
    Success,
    Error,
    Warning,
    Muted,
    Dim,
    Text,
    ToolTitle,
    ToolOutput,
}

impl ThemeColor {
    pub fn as_str(self) -> &'static str {
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
            Self::ToolTitle => "toolTitle",
            Self::ToolOutput => "toolOutput",
        }
    }
}

/// `statusThemeColor` (tools/task/renderers.ts): shared by every tool renderer.
pub fn status_theme_color(status: &str) -> ThemeColor {
    match status {
        "completed" => ThemeColor::Success,
        "error" | "lost" | "invalid_arguments" => ThemeColor::Error,
        "cancelled" | "interrupted" => ThemeColor::Warning,
        "running" => ThemeColor::Accent,
        _ => ThemeColor::Muted,
    }
}

/// The `Pick<Theme, "fg" | "italic">` slice the renderers use.
pub trait RendererTheme {
    fn fg(&self, color: ThemeColor, text: &str) -> String;
    fn italic(&self, text: &str) -> String;
}

/// senpi `ToolRenderResultOptions`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ToolRenderResultOptions {
    pub expanded: bool,
    pub is_partial: bool,
}

/// One `content` entry of an `AgentToolResult` (senpi-task tools only emit text).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ToolContent {
    Text { text: String },
}

/// senpi `AgentToolResult<TDetails>`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AgentToolResult<D> {
    pub content: Vec<ToolContent>,
    pub details: D,
}

impl<D> AgentToolResult<D> {
    /// The first text block (`result.content[0].text` in the TS tests).
    pub fn text(&self) -> &str {
        match self.content.first() {
            Some(ToolContent::Text { text }) => text,
            None => "",
        }
    }
}

/// A rendered tool call/result: `{ render(width): string[]; invalidate(): void }`.
pub trait LinesComponent {
    fn render(&self, width: usize) -> Vec<String>;
    fn invalidate(&self) {}
}

/// `linesComponent(lines)`: static lines are width-truncated with an ellipsis; width-aware lines
/// are rendered as returned. Width 0 yields one empty string per line.
pub enum Lines {
    Static(Vec<String>),
    WidthAware(Box<dyn Fn(usize) -> Vec<String> + Send + Sync>),
}

pub struct LinesView {
    lines: Lines,
}

pub fn lines_component(lines: Lines) -> LinesView {
    LinesView { lines }
}

impl LinesComponent for LinesView {
    fn render(&self, width: usize) -> Vec<String> {
        match &self.lines {
            Lines::Static(lines) => lines
                .iter()
                .map(|line| if width == 0 { String::new() } else { truncate_to_width(line, width, crate::renderer_text::ELLIPSIS) })
                .collect(),
            Lines::WidthAware(lines) => lines(width).into_iter().map(|line| if width == 0 { String::new() } else { line }).collect(),
        }
    }
}

/// Serializes tool details to the JSON the model sees (`details` is plain data in TS).
pub fn details_json<D: Serialize>(details: &D) -> Value {
    serde_json::to_value(details).unwrap_or(Value::Null)
}

// ---------------------------------------------------------------------------------------------
// pi-tui `truncateToWidth` (packages/tui/src/utils.ts), without padding (senpi-task never pads).
// Width uses the crate's per-char `renderer_visible_width`, the same N/A recorded for
// renderer-text in the slice A ledger.
// ---------------------------------------------------------------------------------------------

const RESET: &str = "\x1b[0m";

fn is_printable_ascii(text: &str) -> bool {
    text.bytes().all(|byte| (0x20..=0x7e).contains(&byte))
}

/// `extractAnsiCode(str, pos)` over a char slice: the byte length of a CSI/OSC/DCS/APC sequence.
pub(crate) fn ansi_code_length(chars: &[char], pos: usize) -> Option<usize> {
    extract_ansi_code(chars, pos)
}

fn extract_ansi_code(chars: &[char], pos: usize) -> Option<usize> {
    if chars.get(pos) != Some(&'\x1b') {
        return None;
    }
    let next = *chars.get(pos + 1)?;
    match next {
        '[' => {
            let mut j = pos + 2;
            while j < chars.len() && !matches!(chars[j], 'm' | 'G' | 'K' | 'H' | 'J') {
                j += 1;
            }
            (j < chars.len()).then_some(j + 1 - pos)
        }
        ']' | '_' => {
            let mut j = pos + 2;
            while j < chars.len() {
                if chars[j] == '\x07' {
                    return Some(j + 1 - pos);
                }
                if chars[j] == '\x1b' && chars.get(j + 1) == Some(&'\\') {
                    return Some(j + 2 - pos);
                }
                j += 1;
            }
            None
        }
        'P' => {
            let mut j = pos + 2;
            while j < chars.len() {
                if chars[j] == '\x1b' {
                    if chars.get(j + 1) == Some(&'\\') {
                        return Some(j + 2 - pos);
                    }
                    if chars.get(j + 1) == Some(&'\x1b') {
                        j += 2;
                        continue;
                    }
                }
                j += 1;
            }
            None
        }
        _ => None,
    }
}

fn char_width(ch: char) -> usize {
    renderer_visible_width(ch.encode_utf8(&mut [0; 4]))
}

fn active_osc8_close(prefix: &str) -> &'static str {
    if !prefix.contains("\x1b]8;") {
        return "";
    }
    let chars: Vec<char> = prefix.chars().collect();
    let mut active: Option<bool> = None; // Some(true) = BEL terminator, Some(false) = ST.
    let mut i = 0;
    while i < chars.len() {
        if let Some(len) = extract_ansi_code(&chars, i) {
            let code: String = chars[i..i + len].iter().collect();
            if let Some(body) = code.strip_prefix("\x1b]8;") {
                let bel = code.ends_with('\x07');
                let body = if bel { &body[..body.len() - 1] } else { body.strip_suffix("\x1b\\").unwrap_or(body) };
                if let Some(separator) = body.find(';') {
                    active = if body[separator + 1..].is_empty() { None } else { Some(bel) };
                }
            }
            i += len;
        } else {
            i += 1;
        }
    }
    match active {
        Some(true) => "\x1b]8;;\x07",
        Some(false) => "\x1b]8;;\x1b\\",
        None => "",
    }
}

fn finalize_truncated(prefix: &str, ellipsis: &str) -> String {
    let close = active_osc8_close(prefix);
    if ellipsis.is_empty() { format!("{prefix}{close}{RESET}") } else { format!("{prefix}{close}{RESET}{ellipsis}{RESET}") }
}

fn visible_width(text: &str) -> usize {
    renderer_visible_width(text)
}

fn truncate_fragment(text: &str, max_width: usize) -> (String, usize) {
    let mut out = String::new();
    let mut width = 0;
    for ch in text.chars() {
        let w = char_width(ch);
        if width + w > max_width {
            break;
        }
        out.push(ch);
        width += w;
    }
    (out, width)
}

pub fn truncate_to_width(text: &str, max_width: usize, ellipsis: &str) -> String {
    if max_width == 0 || text.is_empty() {
        return String::new();
    }
    let ellipsis_width = visible_width(ellipsis);
    if ellipsis_width >= max_width {
        if visible_width(text) <= max_width {
            return text.to_string();
        }
        let (clipped, clipped_width) = truncate_fragment(ellipsis, max_width);
        if clipped_width == 0 {
            return String::new();
        }
        return finalize_truncated("", &clipped);
    }
    if is_printable_ascii(text) {
        if text.len() <= max_width {
            return text.to_string();
        }
        return finalize_truncated(&text[..max_width - ellipsis_width], ellipsis);
    }

    let target_width = max_width - ellipsis_width;
    let chars: Vec<char> = text.chars().collect();
    let mut result = String::new();
    let mut pending_ansi = String::new();
    let mut visible_so_far = 0;
    let mut kept_width = 0;
    let mut keep_prefix = true;
    let mut overflowed = false;
    let mut i = 0;
    while i < chars.len() {
        if let Some(len) = extract_ansi_code(&chars, i) {
            pending_ansi.extend(&chars[i..i + len]);
            i += len;
            continue;
        }
        let ch = chars[i];
        let width = char_width(ch);
        if keep_prefix && kept_width + width <= target_width {
            result.push_str(&pending_ansi);
            pending_ansi.clear();
            result.push(ch);
            kept_width += width;
        } else {
            keep_prefix = false;
            pending_ansi.clear();
        }
        visible_so_far += width;
        if visible_so_far > max_width {
            overflowed = true;
            break;
        }
        i += 1;
    }
    if !overflowed {
        return text.to_string();
    }
    finalize_truncated(&result, ellipsis)
}

#[cfg(test)]
pub(crate) mod test_theme {
    use super::{RendererTheme, ThemeColor};

    /// The `ANSI_THEME` fixture the TS renderer tests share.
    pub struct AnsiTheme;

    impl RendererTheme for AnsiTheme {
        fn fg(&self, _color: ThemeColor, text: &str) -> String {
            format!("\u{1b}[33m{text}\u{1b}[0m")
        }
        fn italic(&self, text: &str) -> String {
            format!("\u{1b}[3m{text}\u{1b}[0m")
        }
    }

    /// A theme that tags output with the color name, for asserting which color was chosen.
    #[allow(dead_code)]
    pub struct TagTheme;

    impl RendererTheme for TagTheme {
        fn fg(&self, color: ThemeColor, text: &str) -> String {
            format!("<{}>{text}</{}>", color.as_str(), color.as_str())
        }
        fn italic(&self, text: &str) -> String {
            format!("<i>{text}</i>")
        }
    }
}
