//! Footer v7 layout primitives: colored run groups joined by plain gaps, path elision, and the
//! status-line fold planner. Successor of the footer-layout.ts port (see docs/divergences.md).
use crate::theme::theme::{Theme, ThemeColor};
use maho_tui::utils::{truncate_to_width, visible_width};
#[derive(Debug, Clone, Default)]
pub struct FooterSegment {
    pub plain: String,
    pub colored: String,
}
/// One visual group: adjacent colored runs with no separator between them.
#[derive(Debug, Clone, Default)]
pub struct FooterGroup {
    pub runs: Vec<(String, ThemeColor)>,
}
impl FooterGroup {
    pub fn run(text: impl Into<String>, color: ThemeColor) -> Self {
        Self::default().with(text, color)
    }
    /// Appends a run; empty text is skipped so optional parts can be passed unconditionally.
    pub fn with(mut self, text: impl Into<String>, color: ThemeColor) -> Self {
        let text = text.into();
        if !text.is_empty() {
            self.runs.push((text, color));
        }
        self
    }
    pub fn plain(&self) -> String {
        self.runs.iter().map(|(text, _)| text.as_str()).collect()
    }
    pub fn width(&self) -> usize {
        self.runs.iter().map(|(text, _)| visible_width(text)).sum()
    }
    pub fn render(&self, theme: &Theme) -> String {
        self.runs
            .iter()
            .map(|(text, color)| theme.fg(*color, text))
            .collect()
    }
    /// Keeps the leading `max_width` columns, preserving each run's color.
    pub fn truncated(&self, max_width: usize) -> Self {
        let mut out = Self::default();
        let mut remaining = max_width;
        for (text, color) in &self.runs {
            let kept = truncate_to_width(text, remaining, "", false);
            let kept = maho_tui::utils::strip_terminal_sequences(&kept);
            remaining = remaining.saturating_sub(visible_width(&kept));
            out = out.with(kept, *color);
            if remaining == 0 {
                break;
            }
        }
        out
    }
}
pub fn groups_width(groups: &[FooterGroup], gap: usize) -> usize {
    groups.iter().map(FooterGroup::width).sum::<usize>() + gap * groups.len().saturating_sub(1)
}
/// Joins groups with `gap` uncolored spaces.
pub fn join_groups(groups: &[FooterGroup], gap: usize, theme: &Theme) -> FooterSegment {
    let sep = " ".repeat(gap);
    FooterSegment {
        plain: groups
            .iter()
            .map(FooterGroup::plain)
            .collect::<Vec<_>>()
            .join(&sep),
        colored: groups
            .iter()
            .map(|group| group.render(theme))
            .collect::<Vec<_>>()
            .join(&sep),
    }
}
/// Flattens groups into one group, with the gaps as muted space runs (used for truncation).
pub fn flatten_groups(groups: &[FooterGroup], gap: usize) -> FooterGroup {
    let mut out = FooterGroup::default();
    for (i, group) in groups.iter().enumerate() {
        if i > 0 {
            out = out.with(" ".repeat(gap), ThemeColor::Muted);
        }
        out.runs.extend(group.runs.iter().cloned());
    }
    out
}
pub fn elide_head(text: &str, max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }
    if visible_width(text) <= max_width {
        return text.into();
    }
    if max_width == 1 {
        return "…".into();
    }
    let mut kept = Vec::new();
    let mut used = 0;
    for c in text.chars().rev() {
        let w = visible_width(&c.to_string());
        if used + w > max_width - 1 {
            break;
        }
        kept.push(c);
        used += w;
    }
    format!("…{}", kept.into_iter().rev().collect::<String>())
}
/// Shortens a `~/`- or `/`-rooted path to its last two components: `~/a/b/c/d` -> `~/…/c/d`.
pub fn compact_cwd(path: &str) -> String {
    let (root, rest) = if let Some(rest) = path.strip_prefix("~/") {
        ("~/", rest)
    } else if let Some(rest) = path.strip_prefix('/') {
        ("/", rest)
    } else {
        return path.into();
    };
    let parts: Vec<&str> = rest.split('/').filter(|p| !p.is_empty()).collect();
    if parts.len() <= 2 {
        return path.into();
    }
    format!("{root}…/{}", parts[parts.len() - 2..].join("/"))
}
/// Head-elides a path on component boundaries (`…/project/ferryx`, then `…/ferryx`), falling back to
/// character elision when even the last component is too wide.
pub fn elide_path(path: &str, max_width: usize) -> String {
    if visible_width(path) <= max_width {
        return path.into();
    }
    let parts: Vec<&str> = path
        .split('/')
        .filter(|p| !p.is_empty() && *p != "…")
        .collect();
    for keep in (1..parts.len()).rev() {
        let candidate = format!("…/{}", parts[parts.len() - keep..].join("/"));
        if visible_width(&candidate) <= max_width {
            return candidate;
        }
    }
    elide_head(path, max_width)
}
#[derive(Debug, Clone)]
pub struct FoldableStatus {
    pub text: String,
    /// `None` never folds; otherwise lower priorities fold first.
    pub fold_priority: Option<u8>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusFold {
    /// Indices into the input, in input order.
    pub kept: Vec<usize>,
    /// Number of folded statuses, rendered as `+N`.
    pub folded: usize,
}
/// Width of the kept statuses plus the `+N` marker, joined with `gap` spaces.
pub fn status_fold_width(statuses: &[FoldableStatus], fold: &StatusFold, gap: usize) -> usize {
    let mut widths: Vec<usize> = fold
        .kept
        .iter()
        .map(|&i| visible_width(&statuses[i].text))
        .collect();
    if fold.folded > 0 {
        widths.push(format!("+{}", fold.folded).len());
    }
    widths.iter().sum::<usize>() + gap * widths.len().saturating_sub(1)
}
/// Folds the lowest-priority statuses (later ones first on ties) until the joined line, including
/// the `+N` marker, fits `budget`. Stops when only never-fold statuses remain.
pub fn plan_status_fold(statuses: &[FoldableStatus], budget: usize, gap: usize) -> StatusFold {
    let mut fold = StatusFold {
        kept: (0..statuses.len()).collect(),
        folded: 0,
    };
    while status_fold_width(statuses, &fold, gap) > budget {
        let Some(pos) = fold
            .kept
            .iter()
            .enumerate()
            .filter_map(|(pos, &i)| {
                statuses[i]
                    .fold_priority
                    .map(|p| (p, std::cmp::Reverse(i), pos))
            })
            .min()
            .map(|(_, _, pos)| pos)
        else {
            break;
        };
        fold.kept.remove(pos);
        fold.folded += 1;
    }
    fold
}
