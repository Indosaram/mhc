//! System prompt rendering, external memory projections, and sentinel block wrappers.
//!
//! Spec: `docs/cleanroom/specs/memory-prompt-render.md`.

use crate::redact::redact_secret_like_material;
use std::collections::BTreeMap;
use std::fmt;

const MEMORY_DIR: &str = "$MEMORY_DIR";

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
}

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

/// Render system markdown files into a nested `<tag>` hierarchy.
///
/// The tree comes from the file paths, so `system/human/prefs/coding.md` nests inside `human` and
/// `prefs`; children are emitted in byte order of their labels.
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

        let safe_label = redact_secret_like_material(label);
        lines.push(format!("{pad}<{safe_label}>"));
        if let Some(file) = &child.file {
            let safe_path = child_parts
                .iter()
                .map(|part| redact_secret_like_material(part))
                .collect::<Vec<_>>()
                .join("/");
            lines.push(format!(
                "{pad}  <projection>{MEMORY_DIR}/system/{safe_path}.md</projection>"
            ));
            let description = file.description.trim();
            if !description.is_empty() {
                lines.push(format!("{pad}  <description>{description}</description>"));
            }
            let body = file.body.trim_end();
            if !body.is_empty() {
                lines.push(format!("{pad}  {body}"));
            }
        }
        render_system_node(child, lines, indent + 1, &child_parts);
        lines.push(format!("{pad}</{safe_label}>"));
    }
}

/// Render a tree of memory paths that live outside `system/`.
pub fn render_external_projection(paths: &[String]) -> String {
    let mut root = ProjectionTreeNode::default();
    for path in paths {
        let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        let mut node = &mut root;
        for part in parts {
            node = node.children.entry(part.to_string()).or_default();
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
        match (a_dir, b_dir) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a_name.cmp(b_name),
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

/// Replace the marked block for the identity `sentinel_block` names; append when the prompt has none.
///
/// Only the region between that identity's own sentinels is replaced. A begin marker with no matching
/// end leaves the rest of the prompt untouched rather than deleting to the end of the document.
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

/// Remove every marked memory block from a prompt and trim the surrounding whitespace.
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
mod tests {
    use super::*;

    fn file(relative_path: &str, description: &str, body: &str) -> CompiledSystemFile {
        CompiledSystemFile {
            relative_path: relative_path.to_owned(),
            body: body.to_owned(),
            description: description.to_owned(),
        }
    }

    #[test]
    fn a_flat_file_renders_one_tag() {
        let rendered = render_system_tree(&[file("system/persona.md", "", "the soul")]);
        assert_eq!(
            rendered,
            "<persona>\n  <projection>$MEMORY_DIR/system/persona.md</projection>\n  the soul\n</persona>"
        );
    }

    #[test]
    fn a_description_sits_above_the_body() {
        let rendered = render_system_tree(&[file("system/human.md", "the person", "body")]);
        assert!(
            rendered.contains("<description>the person</description>"),
            "{rendered}"
        );
        let description_at = rendered.find("<description>").unwrap();
        let body_at = rendered.find("  body\n").unwrap();
        assert!(description_at < body_at);
    }

    #[test]
    fn an_empty_description_is_omitted() {
        let rendered = render_system_tree(&[file("system/a.md", "   ", "body")]);
        assert!(!rendered.contains("<description>"), "{rendered}");
    }

    #[test]
    fn an_empty_body_is_omitted() {
        let rendered = render_system_tree(&[file("system/a.md", "d", "  \n ")]);
        assert!(!rendered.contains("  \n"), "{rendered}");
    }

    #[test]
    fn nested_paths_become_nested_tags() {
        let rendered = render_system_tree(&[file("system/human/prefs/coding.md", "", "b")]);
        assert!(rendered.starts_with("<human>\n"), "{rendered}");
        assert!(rendered.contains("  <prefs>\n"), "{rendered}");
        assert!(rendered.contains("    <coding>\n"), "{rendered}");
        assert!(
            rendered.contains("$MEMORY_DIR/system/human/prefs/coding.md"),
            "{rendered}"
        );
        assert!(
            rendered.ends_with("</coding>\n  </prefs>\n</human>"),
            "{rendered}"
        );
    }

    #[test]
    fn children_render_in_byte_order() {
        let rendered = render_system_tree(&[
            file("system/zeta.md", "", "z"),
            file("system/alpha.md", "", "a"),
        ]);
        assert!(rendered.find("<alpha>").unwrap() < rendered.find("<zeta>").unwrap());
    }

    #[test]
    fn a_parent_file_renders_before_its_children() {
        let rendered = render_system_tree(&[
            file("system/human.md", "", "top"),
            file("system/human/prefs.md", "", "inner"),
        ]);
        let parent_body = rendered.find("  top\n").unwrap();
        let child_tag = rendered.find("  <prefs>").unwrap();
        assert!(parent_body < child_tag, "{rendered}");
    }

    #[test]
    fn a_missing_system_prefix_is_tolerated() {
        let rendered = render_system_tree(&[file("persona.md", "", "b")]);
        assert!(rendered.starts_with("<persona>"), "{rendered}");
    }

    #[test]
    fn a_secret_like_label_is_redacted() {
        let rendered = render_system_tree(&[file("system/sk-abcdefghijklmnop.md", "", "b")]);
        assert!(!rendered.contains("abcdefghijklmnop"), "{rendered}");
    }

    #[test]
    fn a_secret_like_label_and_path_render_as_stars() {
        let rendered = render_system_tree(&[file(
            "system/notes-sk-live-ABC123/sk-live-DEF456.md",
            "",
            "body",
        )]);
        assert_eq!(
            rendered,
            [
                "<notes-***>",
                "  <***>",
                "    <projection>$MEMORY_DIR/system/notes-***/***.md</projection>",
                "    body",
                "  </***>",
                "</notes-***>",
            ]
            .join("\n")
        );
    }

    #[test]
    fn a_secret_like_body_is_not_redacted() {
        let rendered = render_system_tree(&[file(
            "system/clean/readme.md",
            "",
            "keep sk-live-ABC123 raw",
        )]);
        assert_eq!(
            rendered,
            [
                "<clean>",
                "  <readme>",
                "    <projection>$MEMORY_DIR/system/clean/readme.md</projection>",
                "    keep sk-live-ABC123 raw",
                "  </readme>",
                "</clean>",
            ]
            .join("\n")
        );
    }

    #[test]
    fn a_clean_label_and_path_render_unchanged() {
        let rendered = render_system_tree(&[file("system/clean/notes.md", "", "plain")]);
        assert_eq!(
            rendered,
            [
                "<clean>",
                "  <notes>",
                "    <projection>$MEMORY_DIR/system/clean/notes.md</projection>",
                "    plain",
                "  </notes>",
                "</clean>",
            ]
            .join("\n")
        );
    }

    #[test]
    fn an_empty_file_list_renders_nothing() {
        assert_eq!(render_system_tree(&[]), "");
    }

    #[test]
    fn the_projection_header_always_renders() {
        let rendered = render_external_projection(&[]);
        assert_eq!(
            rendered,
            "<external_projection>\n$MEMORY_DIR/\n</external_projection>"
        );
    }

    #[test]
    fn directories_sort_before_files() {
        let rendered = render_external_projection(&[
            "zzz/a.md".to_owned(),
            "aaa/b.md".to_owned(),
            "file.md".to_owned(),
        ]);
        let directory_at = rendered.find("aaa/").unwrap();
        let file_at = rendered.find("file.md").unwrap();
        assert!(directory_at < file_at, "{rendered}");
    }

    #[test]
    fn nested_projection_paths_nest_their_branches() {
        let rendered = render_external_projection(&["notes/facts/x.md".to_owned()]);
        assert!(rendered.contains("notes/"), "{rendered}");
        assert!(rendered.contains("facts/"), "{rendered}");
        assert!(rendered.contains("x.md"), "{rendered}");
    }

    #[test]
    fn sibling_projection_paths_share_their_parent() {
        let rendered =
            render_external_projection(&["notes/a.md".to_owned(), "notes/b.md".to_owned()]);
        assert_eq!(rendered.matches("notes/").count(), 1, "{rendered}");
    }

    #[test]
    fn the_last_branch_is_marked() {
        let rendered = render_external_projection(&["a.md".to_owned(), "b.md".to_owned()]);
        assert!(rendered.contains("├── a.md"), "{rendered}");
        assert!(rendered.contains("└── b.md"), "{rendered}");
    }

    #[test]
    fn marking_wraps_the_block() {
        let marked = mark_memory_block("agent", "body");
        assert_eq!(
            marked,
            "<!-- senpi-memory:agent:begin -->\nbody\n<!-- senpi-memory:agent:end -->"
        );
    }

    #[test]
    fn replacing_appends_when_the_prompt_has_no_block() {
        let block = mark_memory_block("a", "new");
        let replaced = replace_memory_block("prompt\n\n", &block).unwrap();
        assert_eq!(
            replaced,
            "prompt\n\n<!-- senpi-memory:a:begin -->\nnew\n<!-- senpi-memory:a:end -->"
        );
    }

    #[test]
    fn replacing_swaps_only_the_marked_region() {
        let prompt =
            "before\n<!-- senpi-memory:a:begin -->\nold\n<!-- senpi-memory:a:end -->\nafter";
        let replaced = replace_memory_block(prompt, &mark_memory_block("a", "new")).unwrap();
        assert_eq!(
            replaced,
            "before\n<!-- senpi-memory:a:begin -->\nnew\n<!-- senpi-memory:a:end -->\nafter"
        );
    }

    #[test]
    fn replacing_leaves_another_identity_block_alone() {
        let prompt = "<!-- senpi-memory:b:begin -->\nb\n<!-- senpi-memory:b:end -->";
        let replaced = replace_memory_block(prompt, &mark_memory_block("a", "new")).unwrap();
        assert!(replaced.contains("<!-- senpi-memory:b:begin -->"));
        assert!(replaced.contains("<!-- senpi-memory:a:begin -->"));
    }

    #[test]
    fn replacing_a_block_without_an_end_leaves_the_tail() {
        let prompt = "start\n<!-- senpi-memory:a:begin -->\nno end here";
        let replaced = replace_memory_block(prompt, &mark_memory_block("a", "new")).unwrap();
        assert!(replaced.ends_with("no end here"), "{replaced}");
        assert!(!replaced.contains("new"), "{replaced}");
    }

    #[test]
    fn replacing_rejects_a_block_without_a_begin_sentinel() {
        let error = replace_memory_block("prompt", "not a block").unwrap_err();
        assert_eq!(error, RenderError::MissingBeginSentinel);
    }

    #[test]
    fn replacing_rejects_an_empty_identity() {
        let error = replace_memory_block("prompt", "<!-- senpi-memory::begin -->\nx").unwrap_err();
        assert_eq!(error, RenderError::MissingBeginSentinel);
    }

    #[test]
    fn replacing_rejects_an_identity_with_a_colon() {
        let block = "<!-- senpi-memory:a:b:begin -->\nx";
        assert_eq!(
            replace_memory_block("prompt", block).unwrap_err(),
            RenderError::MissingBeginSentinel
        );
    }

    #[test]
    fn stripping_removes_the_block_and_trims() {
        let prompt =
            "  before\n<!-- senpi-memory:a:begin -->\nbody\n<!-- senpi-memory:a:end -->\nafter  ";
        assert_eq!(strip_memory_block(prompt), "before\n\nafter");
    }

    #[test]
    fn stripping_removes_several_identities() {
        let prompt = "<!-- senpi-memory:a:begin -->\na\n<!-- senpi-memory:a:end -->mid<!-- senpi-memory:b:begin -->\nb\n<!-- senpi-memory:b:end -->";
        assert_eq!(strip_memory_block(prompt), "mid");
    }

    #[test]
    fn stripping_keeps_an_unterminated_marker() {
        let prompt = "text <!-- senpi-memory:a:begin --> dangling";
        assert_eq!(
            strip_memory_block(prompt),
            "text <!-- senpi-memory:a:begin --> dangling"
        );
    }

    #[test]
    fn stripping_a_prompt_without_markers_returns_it_trimmed() {
        assert_eq!(strip_memory_block("  plain  "), "plain");
    }

    #[test]
    fn marking_then_stripping_is_identity() {
        let prompt = "keep me";
        let marked = mark_memory_block("a", "body");
        assert_eq!(strip_memory_block(&format!("{prompt}\n{marked}")), prompt);
    }
}
