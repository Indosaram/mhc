//! System prompt rendering, external memory projections, and sentinel block wrappers.

use std::collections::BTreeMap;
use std::fmt;

const MEMORY_DIR: &str = "$MEMORY_DIR";

/// A system markdown file prepared for prompt compilation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledSystemFile {
    pub relative_path: String,
    pub body: String,
    pub description: String,
}

#[derive(Default)]
struct SystemTreeNode {
    children: BTreeMap<String, SystemTreeNode>,
    file: Option<CompiledSystemFile>,
}

#[derive(Default)]
struct ProjectionTreeNode {
    children: BTreeMap<String, ProjectionTreeNode>,
    leaf: bool,
}

/// Errors raised during sentinel block parsing and replacement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenderError {
    MissingBeginSentinel,
}

impl fmt::Display for RenderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingBeginSentinel => {
                formatter.write_str("Memory block is missing a valid begin sentinel")
            }
        }
    }
}

impl std::error::Error for RenderError {}

/// Render system markdown files into nested `<tag>` hierarchies.
pub fn render_system_tree(files: &[CompiledSystemFile]) -> String {
    let mut root = SystemTreeNode::default();
    for file in files {
        let stripped = file
            .relative_path
            .strip_prefix("system/")
            .unwrap_or(&file.relative_path);
        let stripped = stripped.strip_suffix(".md").unwrap_or(stripped);
        let parts: Vec<&str> = stripped.split('/').filter(|s| !s.is_empty()).collect();

        let mut node = &mut root;
        for part in parts {
            node = node.children.entry(part.to_string()).or_default();
        }
        node.file = Some(file.clone());
    }

    let mut lines = Vec::new();
    render_system_node(&root, &mut lines, 0, &[]);
    lines.join("\n")
}

fn render_system_node(
    node: &SystemTreeNode,
    lines: &mut Vec<String>,
    indent: usize,
    path_parts: &[&str],
) {
    let pad = "  ".repeat(indent);
    for (label, child) in &node.children {
        let mut child_parts = path_parts.to_vec();
        child_parts.push(label.as_str());

        lines.push(format!("{pad}<{label}>"));
        if let Some(file) = &child.file {
            lines.push(format!(
                "{pad}  <projection>{MEMORY_DIR}/system/{}.md</projection>",
                child_parts.join("/")
            ));
            let desc_trimmed = file.description.trim();
            if !desc_trimmed.is_empty() {
                lines.push(format!("{pad}  <description>{desc_trimmed}</description>"));
            }
            let body_trimmed = file.body.trim_end();
            if !body_trimmed.is_empty() {
                lines.push(format!("{pad}  {body_trimmed}"));
            }
        }
        render_system_node(child, lines, indent + 1, &child_parts);
        lines.push(format!("{pad}</{label}>"));
    }
}

/// Render a tree projection for external files outside `system/` and `skills/`.
pub fn render_external_projection(paths: &[String]) -> String {
    let mut root = ProjectionTreeNode::default();
    for path in paths {
        let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        let mut node = &mut root;
        let count = parts.len();
        for (index, part) in parts.into_iter().enumerate() {
            node = node.children.entry(part.to_string()).or_default();
            if index + 1 == count {
                node.leaf = true;
            }
        }
    }

    let mut lines = vec![
        "<external_projection>".to_string(),
        format!("{MEMORY_DIR}/"),
    ];
    render_projection_node(&root, &mut lines, "");
    lines.push("</external_projection>".to_string());
    lines.join("\n")
}

fn render_projection_node(node: &ProjectionTreeNode, lines: &mut Vec<String>, prefix: &str) {
    let mut entries: Vec<(&String, &ProjectionTreeNode)> = node.children.iter().collect();
    entries.sort_by(|(a_name, a), (b_name, b)| {
        let a_dir = !a.children.is_empty();
        let b_dir = !b.children.is_empty();
        if a_dir != b_dir {
            if a_dir {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Greater
            }
        } else {
            a_name.cmp(b_name)
        }
    });

    let count = entries.len();
    for (index, (name, child)) in entries.into_iter().enumerate() {
        let last = index + 1 == count;
        let directory = !child.children.is_empty();
        let branch = if last { "└── " } else { "├── " };
        let slash = if directory { "/" } else { "" };
        lines.push(format!("{prefix}{branch}{name}{slash}"));
        if directory {
            let next_prefix = format!("{prefix}{}", if last { "    " } else { "│   " });
            render_projection_node(child, lines, &next_prefix);
        }
    }
}

/// Wrap a compiled memory block with begin and end sentinels for an agent identity.
pub fn mark_memory_block(identity: &str, block: &str) -> String {
    format!("<!-- senpi-memory:{identity}:begin -->\n{block}\n<!-- senpi-memory:{identity}:end -->")
}

/// Replace matching marked memory block regions in a prompt or append at the end.
pub fn replace_memory_block(prompt: &str, sentinel_block: &str) -> Result<String, RenderError> {
    let identity = extract_sentinel_identity(sentinel_block)?;
    let begin_marker = format!("<!-- senpi-memory:{identity}:begin -->");
    let end_marker = format!("<!-- senpi-memory:{identity}:end -->");

    if !prompt.contains(&begin_marker) {
        return Ok(format!("{}\n\n{sentinel_block}", prompt.trim_end()));
    }

    let mut result = String::with_capacity(prompt.len() + sentinel_block.len());
    let mut cursor = 0;

    while let Some(begin_idx) = prompt[cursor..].find(&begin_marker) {
        let absolute_begin = cursor + begin_idx;
        result.push_str(&prompt[cursor..absolute_begin]);

        let after_begin = absolute_begin + begin_marker.len();
        if let Some(end_idx) = prompt[after_begin..].find(&end_marker) {
            let absolute_end = after_begin + end_idx + end_marker.len();
            result.push_str(sentinel_block);
            cursor = absolute_end;
        } else {
            result.push_str(&prompt[absolute_begin..]);
            cursor = prompt.len();
            break;
        }
    }

    result.push_str(&prompt[cursor..]);
    Ok(result)
}

/// Strip any marked memory block regions from a prompt and trim whitespace.
pub fn strip_memory_block(prompt: &str) -> String {
    let prefix = "<!-- senpi-memory:";
    let begin_suffix = ":begin -->";
    let end_suffix = ":end -->";

    let mut result = String::with_capacity(prompt.len());
    let mut cursor = 0;

    while let Some(start_offset) = prompt[cursor..].find(prefix) {
        let block_start = cursor + start_offset;
        let id_start = block_start + prefix.len();

        if let Some(begin_offset) = prompt[id_start..].find(begin_suffix) {
            let identity = &prompt[id_start..id_start + begin_offset];
            if !identity.is_empty() && !identity.contains([':', '\r', '\n']) {
                let after_begin = id_start + begin_offset + begin_suffix.len();
                let end_marker = format!("{prefix}{identity}{end_suffix}");
                if let Some(end_offset) = prompt[after_begin..].find(&end_marker) {
                    let block_end = after_begin + end_offset + end_marker.len();
                    result.push_str(&prompt[cursor..block_start]);
                    cursor = block_end;
                    continue;
                }
            }
        }

        result.push_str(&prompt[cursor..block_start + prefix.len()]);
        cursor = block_start + prefix.len();
    }

    result.push_str(&prompt[cursor..]);
    result.trim().to_string()
}

fn extract_sentinel_identity(block: &str) -> Result<String, RenderError> {
    let prefix = "<!-- senpi-memory:";
    let begin_suffix = ":begin -->";

    if let Some(stripped) = block.strip_prefix(prefix)
        && let Some(end_idx) = stripped.find(begin_suffix)
    {
        let identity = &stripped[..end_idx];
        if !identity.is_empty() && !identity.contains([':', '\r', '\n']) {
            return Ok(identity.to_string());
        }
    }
    Err(RenderError::MissingBeginSentinel)
}

#[cfg(test)]
#[path = "render_tests.rs"]
mod tests;
