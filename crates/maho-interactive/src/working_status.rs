//! Port of working-status.ts.
use maho_tui::utils::strip_terminal_sequences;
use serde_json::Value;
pub fn large_session_working_status_interval(
    entries: usize,
    default_ms: u64,
    large_ms: u64,
) -> u64 {
    if entries >= 1000 {
        large_ms
    } else {
        default_ms
    }
}
pub fn format_working_elapsed_seconds(elapsed: f64) -> String {
    let total = elapsed.floor().max(0.) as u64;
    let seconds = total % 60;
    let minutes = total / 60;
    if total < 60 {
        format!("{total}s")
    } else if total < 3600 {
        format!("{minutes}m {seconds:02}s")
    } else {
        format!("{}h {:02}m {seconds:02}s", minutes / 60, minutes % 60)
    }
}
pub fn format_working_status_message(message: &str, elapsed: f64, key: &str) -> String {
    format!(
        "{message} ({} • {key} to interrupt)",
        format_working_elapsed_seconds(elapsed)
    )
}
pub fn format_tool_hook_status_message(hook: &str, status: &str, elapsed: f64) -> String {
    format!(
        "Running {hook} hook{} ({})",
        if status.is_empty() {
            String::new()
        } else {
            format!(": {status}")
        },
        format_working_elapsed_seconds(elapsed)
    )
}
pub fn sanitize_working_status_plain_text(value: &str) -> String {
    strip_terminal_sequences(value)
        .replace(['\r', '\n', '\t'], " ")
        .chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}
pub fn format_active_tool_working_label(name: &str, input: &Value) -> String {
    let name = sanitize_working_status_plain_text(name);
    let name = if name.is_empty() { "tool" } else { &name };
    let detail = input
        .get("command")
        .and_then(Value::as_str)
        .map(sanitize_working_status_plain_text)
        .unwrap_or_default();
    let label = if detail.is_empty() {
        format!("Running {name}")
    } else {
        format!("Running {name}: {detail}")
    };
    if label.chars().count() <= 80 {
        label
    } else {
        format!("{}...", label.chars().take(77).collect::<String>())
    }
}
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WorkingStatusRgbColor {
    pub r: f64,
    pub g: f64,
    pub b: f64,
}
pub fn blend_working_status_shimmer_rgb_color(
    highlight: WorkingStatusRgbColor,
    base: WorkingStatusRgbColor,
    amount: f64,
) -> WorkingStatusRgbColor {
    let a = amount.clamp(0., 1.);
    let blend = |h: f64, b: f64| (h * a + b * (1. - a)).round().clamp(0., 255.);
    WorkingStatusRgbColor {
        r: blend(highlight.r, base.r),
        g: blend(highlight.g, base.g),
        b: blend(highlight.b, base.b),
    }
}
pub type ShimmerStyle = dyn Fn(&str, f64) -> String;
pub struct WorkingStatusTextFrameStyle<'a> {
    pub base: &'a dyn Fn(&str) -> String,
    pub glow: &'a dyn Fn(&str) -> String,
    pub highlight: &'a dyn Fn(&str) -> String,
    pub shimmer: Option<&'a ShimmerStyle>,
}
pub fn format_working_status_text_frame(
    message: &str,
    elapsed_ms: f64,
    style: &WorkingStatusTextFrameStyle<'_>,
) -> String {
    let chars: Vec<_> = message.chars().collect();
    let progress = (elapsed_ms.max(0.) % 2000.) / 2000. * (chars.len() + 20) as f64;
    chars
        .into_iter()
        .enumerate()
        .map(|(i, c)| {
            if c == ' ' {
                return " ".into();
            }
            let text = c.to_string();
            let distance = ((i + 10) as f64 - progress).abs();
            let intensity = if distance > 5. {
                0.
            } else {
                0.5 * (1. + (std::f64::consts::PI * distance / 5.).cos())
            };
            if let Some(shimmer) = style.shimmer {
                shimmer(&text, intensity)
            } else if intensity < 0.2 {
                (style.base)(&text)
            } else if intensity < 0.6 {
                (style.glow)(&text)
            } else {
                (style.highlight)(&text)
            }
        })
        .collect()
}
pub fn format_working_status_message_frame(
    message: &str,
    elapsed: f64,
    key: &str,
    animation_ms: f64,
    style: &WorkingStatusTextFrameStyle<'_>,
    suffix: &dyn Fn(&str) -> String,
) -> String {
    format!(
        "{}{}",
        format_working_status_text_frame(message, animation_ms, style),
        suffix(&format!(
            " ({} • {key} to interrupt)",
            format_working_elapsed_seconds(elapsed)
        ))
    )
}
pub fn format_tool_hook_status_message_frame(
    hook: &str,
    status: &str,
    elapsed: f64,
    animation_ms: f64,
    style: &WorkingStatusTextFrameStyle<'_>,
    suffix: &dyn Fn(&str) -> String,
) -> String {
    format!(
        "{}{}",
        format_working_status_text_frame(&format!("Running {hook} hook"), animation_ms, style),
        suffix(&format!(
            "{} ({})",
            if status.is_empty() {
                String::new()
            } else {
                format!(": {status}")
            },
            format_working_elapsed_seconds(elapsed)
        ))
    )
}
