//! Bounded external-path projection with recency ordering and byte-budget fitting.
use std::collections::BTreeMap;

use super::external_projection_dirs::group_directories;
use super::external_projection_names::{NAME_LEGEND, NAME_NOTE, ProjectionItem, directory_items};

const MEMORY_DIR: &str = "$MEMORY_DIR";
const OPEN: &str = "<external_projection>";
const CLOSE: &str = "</external_projection>";

/// `0` disables a limit; both at `0` reproduces the unbounded, name-ordered listing byte for byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct ExternalProjectionLimits {
    pub max_entries_per_directory: usize,
    pub max_bytes: usize,
}
impl Default for ExternalProjectionLimits {
    fn default() -> Self {
        Self {
            max_entries_per_directory: 0,
            max_bytes: 40_960,
        }
    }
}
#[derive(Debug, Default)]
pub struct ExternalProjectionInput {
    pub times: BTreeMap<String, i64>,
    pub limits: ExternalProjectionLimits,
    pub recency_unavailable: bool,
    pub recency_error: Option<String>,
}
#[derive(Debug, Clone)]
pub struct ExternalProjectionStats {
    pub shown: usize,
    pub omitted: usize,
    pub bytes: usize,
    pub max_bytes: usize,
    pub overflow: bool,
    pub recency_unavailable: bool,
}
struct DirectoryListing {
    label: String,
    pointer: String,
    items: Vec<ProjectionItem>,
    item_bytes: Vec<usize>,
}
struct Layout {
    root_bare: bool,
    /// Some line folds or groups names, so the block opens with the full legend instead of the short note.
    legend: bool,
    directories: Vec<DirectoryListing>,
    allowances: Vec<usize>,
}

fn files(items: &[ProjectionItem]) -> usize {
    items.iter().map(|item| item.files).sum()
}
fn marker(omitted: usize, pointer: &str) -> String {
    format!("(+{omitted} more; read {pointer} to list)")
}
fn layout(paths: &[String], input: Option<&ExternalProjectionInput>) -> Layout {
    let limits = input.map_or(
        ExternalProjectionLimits {
            max_entries_per_directory: 0,
            max_bytes: 0,
        },
        |input| input.limits,
    );
    let bounded = limits.max_entries_per_directory > 0 || limits.max_bytes > 0;
    let groups = group_directories(paths, input.filter(|_| bounded).map(|input| &input.times));
    let directories: Vec<DirectoryListing> = groups
        .iter()
        .map(|group| {
            let items = directory_items(&group.files);
            let item_bytes = items.iter().map(|item| item.text.len()).collect();
            DirectoryListing {
                label: group.label.clone(),
                pointer: group.pointer.clone(),
                items,
                item_bytes,
            }
        })
        .collect();
    let cap = limits.max_entries_per_directory;
    let mut allowances: Vec<usize> = directories
        .iter()
        .map(|directory| {
            if cap > 0 {
                cap.min(directory.items.len())
            } else {
                directory.items.len()
            }
        })
        .collect();
    let root_label = format!("{MEMORY_DIR}/");
    let root_bare = !groups.iter().any(|group| group.label == root_label);
    let legend = groups.iter().any(|group| group.grouped)
        || directories
            .iter()
            .any(|directory| directory.items.iter().any(|item| item.files > 1));
    if limits.max_bytes > 0 {
        fit_to_budget(
            &directories,
            &mut allowances,
            limits.max_bytes,
            root_bare,
            legend,
        );
    }
    Layout {
        root_bare,
        legend,
        directories,
        allowances,
    }
}

fn line_bytes(directories: &[DirectoryListing], index: usize, allowance: usize) -> usize {
    let directory = &directories[index];
    let mut size = format!("{}: ", directory.label).len();
    for shown in 0..allowance {
        size +=
            directory.item_bytes.get(shown).copied().unwrap_or(0) + if shown > 0 { 2 } else { 0 };
    }
    let omitted = files(&directory.items[allowance.min(directory.items.len())..]);
    if omitted > 0 {
        size += usize::from(allowance > 0) + marker(omitted, &directory.pointer).len();
    }
    size
}

/// Shrinks the directory showing the most names by one until the block fits, so large directories
/// give up names first and ties rotate in listing order. Never drops a directory line; when even
/// the floor (every allowance zero) is over budget, the caller reports the overflow.
fn fit_to_budget(
    directories: &[DirectoryListing],
    allowances: &mut [usize],
    max_bytes: usize,
    root_bare: bool,
    legend: bool,
) {
    let start = allowances.to_vec();
    let mut lines: Vec<usize> = (0..directories.len())
        .map(|index| line_bytes(directories, index, allowances[index]))
        .collect();
    let fixed_lines = 3 + usize::from(root_bare);
    let fixed_bytes = OPEN.len()
        + (if legend { NAME_LEGEND } else { NAME_NOTE }).len()
        + CLOSE.len()
        + if root_bare {
            format!("{MEMORY_DIR}/").len()
        } else {
            0
        };
    let mut total = fixed_bytes
        + lines.iter().sum::<usize>()
        + (fixed_lines + directories.len().saturating_sub(1));
    while total > max_bytes {
        let mut widest: Option<usize> = None;
        for index in 0..allowances.len() {
            if allowances[index] > widest.map_or(0, |at| allowances[at]) {
                widest = Some(index);
            }
        }
        let Some(widest) = widest else {
            break;
        };
        allowances[widest] -= 1;
        let before = lines[widest];
        lines[widest] = line_bytes(directories, widest, allowances[widest]);
        total = total + lines[widest] - before;
    }
    if total <= max_bytes {
        return;
    }
    // An omitted-names marker can cost more than the names it replaces, so widest-first shrinking
    // can miss the smallest listing. Lines are independent: each directory at its own smallest
    // line is the true minimum, which may still fit; ties keep more names.
    for (index, limit) in start.into_iter().enumerate() {
        let mut best = limit;
        for allowance in (0..limit).rev() {
            if line_bytes(directories, index, allowance) < line_bytes(directories, index, best) {
                best = allowance;
            }
        }
        allowances[index] = best;
    }
}

fn render(layout: &Layout) -> String {
    let mut lines = vec![
        OPEN.to_owned(),
        (if layout.legend {
            NAME_LEGEND
        } else {
            NAME_NOTE
        })
        .to_owned(),
    ];
    if layout.root_bare {
        lines.push(format!("{MEMORY_DIR}/"));
    }
    for (directory, allowance) in layout.directories.iter().zip(&layout.allowances) {
        let allowance = (*allowance).min(directory.items.len());
        let shown = directory.items[..allowance]
            .iter()
            .map(|item| item.text.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let omitted = files(&directory.items[allowance..]);
        let suffix = if omitted > 0 {
            format!(
                "{}{}",
                if shown.is_empty() { "" } else { " " },
                marker(omitted, &directory.pointer)
            )
        } else {
            String::new()
        };
        lines.push(format!("{}: {shown}{suffix}", directory.label));
    }
    lines.push(CLOSE.to_owned());
    lines.join("\n")
}

/// Renders the projection; no input means both limits disabled and name order.
pub fn render_external_projection_unbounded(paths: &[String]) -> String {
    render(&layout(paths, None))
}
pub fn render_external_projection(paths: &[String], input: &ExternalProjectionInput) -> String {
    render(&layout(paths, Some(input)))
}
pub fn render_external_projection_stats(
    paths: &[String],
    input: &ExternalProjectionInput,
) -> ExternalProjectionStats {
    let fitted = layout(paths, Some(input));
    let bytes = render(&fitted).len();
    let total: usize = fitted.directories.iter().map(|d| files(&d.items)).sum();
    let shown: usize = fitted
        .directories
        .iter()
        .zip(&fitted.allowances)
        .map(|(d, allowance)| files(&d.items[..(*allowance).min(d.items.len())]))
        .sum();
    ExternalProjectionStats {
        shown,
        omitted: total - shown,
        bytes,
        max_bytes: input.limits.max_bytes,
        overflow: input.limits.max_bytes > 0 && bytes > input.limits.max_bytes,
        recency_unavailable: input.recency_unavailable,
    }
}

#[cfg(test)]
#[path = "external_projection_tests.rs"]
mod tests;
