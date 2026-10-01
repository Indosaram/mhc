use super::read_folders::{is_read_summary_path, types::*};
use std::collections::VecDeque;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadSegment {
    Kept {
        start_line: usize,
        end_line: usize,
        text: String,
    },
    Elided {
        start_line: usize,
        end_line: usize,
    },
}
impl ReadSegment {
    pub fn range(&self) -> ReadLineRange {
        match self {
            Self::Kept {
                start_line,
                end_line,
                ..
            }
            | Self::Elided {
                start_line,
                end_line,
            } => ReadLineRange {
                start_line: *start_line,
                end_line: *end_line,
            },
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reread {
    pub offset: usize,
    pub limit: usize,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadFooter {
    pub text: String,
    pub rereads: Vec<Reread>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedReadView {
    pub text: String,
    pub elided_ranges: Vec<ReadLineRange>,
    pub footer: ReadFooter,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SegmentedReadView {
    Summary {
        segments: Vec<ReadSegment>,
        visible_source_lines: usize,
        rendered: RenderedReadView,
    },
    NoSummary {
        reason: String,
    },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("Read segments must cover the source exactly, in order, without altering kept lines")]
pub struct InvalidReadSegmentsError;
impl InvalidReadSegmentsError {
    pub const CODE: &str = "invalid_segments";
}

pub fn create_default_read_summary(
    path: &str,
    text: &str,
    offset: Option<usize>,
    limit: Option<usize>,
    folder: Option<&dyn ReadFolder>,
    truncated: bool,
) -> Option<RenderedReadView> {
    let folder = folder?;
    if offset.is_some()
        || limit.is_some()
        || truncated
        || !is_read_summary_path(path)
        || text.contains('\0')
        || text.split('\n').count() < READ_FOLD_SETTINGS.min_total_lines
    {
        return None;
    }
    match create_segmented_read_view(
        text,
        &folder.fold(ReadFolderInput {
            path,
            text,
            settings: READ_FOLD_SETTINGS,
        }),
    ) {
        SegmentedReadView::Summary { rendered, .. } => Some(rendered),
        SegmentedReadView::NoSummary { .. } => None,
    }
}
fn valid_range(start: usize, end: usize, total: usize) -> bool {
    start >= 1 && end >= start && end <= total
}
pub fn render_segmented_read_view(
    text: &str,
    segments: &[ReadSegment],
) -> Result<RenderedReadView, InvalidReadSegmentsError> {
    let lines: Vec<_> = text.split('\n').collect();
    let mut parts = Vec::new();
    let mut elided_ranges = Vec::new();
    let mut cursor = 1;
    for segment in segments {
        let range = segment.range();
        if !valid_range(range.start_line, range.end_line, lines.len()) || range.start_line != cursor
        {
            return Err(InvalidReadSegmentsError);
        }
        match segment {
            ReadSegment::Kept { text, .. } => {
                if *text != lines[range.start_line - 1..range.end_line].join("\n") {
                    return Err(InvalidReadSegmentsError);
                }
                parts.push(text.clone());
            }
            ReadSegment::Elided { .. } => {
                parts.push("…".into());
                elided_ranges.push(range);
            }
        }
        cursor = range.end_line + 1;
    }
    if cursor != lines.len() + 1 {
        return Err(InvalidReadSegmentsError);
    }
    let rereads: Vec<_> = elided_ranges
        .iter()
        .map(|r| Reread {
            offset: r.start_line,
            limit: r.end_line - r.start_line + 1,
        })
        .collect();
    let footer_text = if rereads.is_empty() {
        String::new()
    } else {
        format!(
            "[Elided source: {}. Reread source before editing; markers are not source.]",
            rereads
                .iter()
                .map(|r| format!("offset={} limit={}", r.offset, r.limit))
                .collect::<Vec<_>>()
                .join("; ")
        )
    };
    if !footer_text.is_empty() {
        parts.push(String::new());
        parts.push(footer_text.clone());
    }
    Ok(RenderedReadView {
        text: parts.join("\n"),
        elided_ranges,
        footer: ReadFooter {
            text: footer_text,
            rereads,
        },
    })
}
fn valid_hierarchy(ranges: &[ReadFoldRange], total: usize) -> bool {
    let mut queue = VecDeque::from([(ranges, 0, total + 1)]);
    while let Some((ranges, start, end)) = queue.pop_front() {
        let mut previous = start;
        for range in ranges {
            if !valid_range(range.start_line, range.end_line, total)
                || range.start_line <= previous
                || range.end_line >= end
            {
                return false;
            }
            previous = range.end_line;
            queue.push_back((&range.children, range.start_line, range.end_line));
        }
    }
    true
}
fn length(range: &ReadFoldRange) -> usize {
    range.end_line - range.start_line + 1
}
pub fn create_segmented_read_view(text: &str, parsed: &ReadFolderResult) -> SegmentedReadView {
    let raw = |reason: &str| SegmentedReadView::NoSummary {
        reason: reason.into(),
    };
    let (source, ranges) = match parsed {
        ReadFolderResult::Unsupported { reason } => return raw(reason),
        ReadFolderResult::ParseFailure { .. } => return raw("parse_failure"),
        ReadFolderResult::Parsed { text, ranges } => (text, ranges),
    };
    if source != text {
        return raw("stale_source");
    }
    let lines: Vec<_> = text.split('\n').collect();
    if !valid_hierarchy(ranges, lines.len()) {
        return raw("invalid_ranges");
    }
    if lines.len() < READ_FOLD_SETTINGS.min_total_lines {
        return raw("too_short");
    }
    if ranges.is_empty() {
        return raw("no_elision");
    }
    let mut visible = lines.len() - ranges.iter().map(length).sum::<usize>();
    if visible > READ_FOLD_SETTINGS.unfold_limit {
        return raw("skeleton_exceeds_budget");
    }
    let mut selected: Vec<_> = ranges.iter().collect();
    let mut queue: VecDeque<_> = ranges.iter().collect();
    while visible < READ_FOLD_SETTINGS.unfold_until {
        let Some(parent) = queue.pop_front() else {
            break;
        };
        let next = visible + length(parent) - parent.children.iter().map(length).sum::<usize>();
        if next > READ_FOLD_SETTINGS.unfold_limit {
            continue;
        }
        selected.retain(|r| !std::ptr::eq(*r, parent));
        selected.extend(parent.children.iter());
        queue.extend(parent.children.iter());
        visible = next;
    }
    if visible < READ_FOLD_SETTINGS.unfold_until {
        return raw("visible_budget_unreachable");
    }
    if selected.is_empty() {
        return raw("no_elision");
    }
    selected.sort_by_key(|r| r.start_line);
    let mut segments = Vec::new();
    let mut cursor = 1;
    for range in selected {
        if cursor < range.start_line {
            segments.push(ReadSegment::Kept {
                start_line: cursor,
                end_line: range.start_line - 1,
                text: lines[cursor - 1..range.start_line - 1].join("\n"),
            });
        }
        segments.push(ReadSegment::Elided {
            start_line: range.start_line,
            end_line: range.end_line,
        });
        cursor = range.end_line + 1;
    }
    if cursor <= lines.len() {
        segments.push(ReadSegment::Kept {
            start_line: cursor,
            end_line: lines.len(),
            text: lines[cursor - 1..].join("\n"),
        });
    }
    let rendered = match render_segmented_read_view(text, &segments) {
        Ok(rendered) => rendered,
        Err(_) => return raw("invalid_ranges"),
    };
    if rendered.text.len() >= text.len() {
        return raw("no_output_saving");
    }
    SegmentedReadView::Summary {
        segments,
        visible_source_lines: visible,
        rendered,
    }
}
