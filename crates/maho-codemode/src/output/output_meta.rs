use std::path::{Path, PathBuf};

pub fn format_middle_elision_marker(elided_lines: usize, elided_bytes: usize) -> String {
    if elided_lines <= 1 { format!("[…{elided_bytes}B elided…]") } else { format!("[…{elided_lines}ln elided…]") }
}
pub fn artifact_notice(path: &str) -> String { format!("[Full output: {path}]") }

pub struct SessionArtifactsDir { pub dir: PathBuf, pub temp: bool }

pub fn resolve_session_artifacts_dir(session_file: Option<&Path>) -> Result<SessionArtifactsDir, std::io::Error> {
    let dir = match session_file {
        Some(path) => {
            let path = path.to_string_lossy();
            PathBuf::from(format!("{}-artifacts", path.strip_suffix(".jsonl").unwrap_or(&path)))
        }
        None => {
            let mut bytes = [0u8; 8];
            getrandom::fill(&mut bytes).map_err(std::io::Error::other)?;
            let id = bytes.iter().map(|byte| format!("{byte:02x}")).collect::<String>();
            std::env::temp_dir().join(format!("senpi-codemode-{id}"))
        }
    };
    std::fs::create_dir_all(&dir)?;
    Ok(SessionArtifactsDir { dir, temp: session_file.is_none() })
}

#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction { Head, Tail, Middle }
#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TruncatedBy { Lines, Bytes, Columns, Middle }
#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
pub struct LineRange { pub start: usize, pub end: usize }

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TruncationMeta {
    pub direction: Direction,
    pub truncated_by: TruncatedBy,
    pub total_lines: usize,
    pub total_bytes: usize,
    pub output_lines: usize,
    pub output_bytes: usize,
    pub max_bytes: Option<usize>,
    pub max_columns: Option<usize>,
    pub column_truncated_lines: Option<usize>,
    pub shown_range: Option<LineRange>,
    pub head_range: Option<LineRange>,
    pub tail_range: Option<LineRange>,
    pub elided_bytes: Option<usize>,
    pub elided_lines: Option<usize>,
    pub artifact_id: Option<String>,
}

fn format_bytes(bytes: usize) -> String {
    if bytes < 1024 { format!("{bytes}B") }
    else if bytes < 1024 * 1024 { format!("{:.1}KB", bytes as f64 / 1024.0) }
    else { format!("{:.1}MB", bytes as f64 / (1024.0 * 1024.0)) }
}

pub fn format_truncation_warning(meta: Option<&TruncationMeta>) -> Option<String> {
    let meta = meta?;
    let mut message = match meta.direction {
        Direction::Middle => match (meta.head_range, meta.tail_range) {
            (Some(head), Some(tail)) => {
                let lines = meta.elided_lines.unwrap_or(meta.total_lines.saturating_sub(meta.output_lines));
                let bytes = meta.elided_bytes.unwrap_or(meta.total_bytes.saturating_sub(meta.output_bytes));
                format!("Showing lines {}-{} and {}-{} of {}; {lines} middle line{} ({}) elided", head.start, head.end, tail.start, tail.end, meta.total_lines, if lines == 1 { "" } else { "s" }, format_bytes(bytes))
            }
            _ => format!("Showing {} of {} lines; middle elided", meta.output_lines, meta.total_lines),
        },
        Direction::Head | Direction::Tail => {
            let mut message = match meta.shown_range.filter(|range| range.end >= range.start) {
                Some(range) => format!("Showing lines {}-{} of {}", range.start, range.end, meta.total_lines),
                None => format!("Showing {} of {} lines", meta.output_lines, meta.total_lines),
            };
            let dropped = format_bytes(meta.total_bytes.saturating_sub(meta.output_bytes));
            match meta.truncated_by {
                TruncatedBy::Columns => {
                    let clamped = meta.column_truncated_lines.unwrap_or(0);
                    let width = meta.max_columns.map_or_else(|| "the column cap".into(), |width| format!("{width} columns"));
                    message.push_str(&format!("; {clamped} line{} clamped to {width} ({dropped} dropped)", if clamped == 1 { "" } else { "s" }));
                }
                TruncatedBy::Bytes => message.push_str(&meta.max_bytes.map_or_else(|| format!(" ({dropped} dropped)"), |bytes| format!(" ({} limit)", format_bytes(bytes)))),
                TruncatedBy::Lines | TruncatedBy::Middle => {}
            }
            message
        }
    };
    if let Some(path) = &meta.artifact_id { message.push_str(&format!(". Full output: {path}")); }
    Some(format!("[{message}]"))
}

pub fn strip_output_notice<'a>(text: &'a str, meta: Option<&TruncationMeta>) -> &'a str {
    let Some(notice) = format_truncation_warning(meta) else { return text; };
    text.trim_end().strip_suffix(&notice).map_or(text, str::trim_end)
}
