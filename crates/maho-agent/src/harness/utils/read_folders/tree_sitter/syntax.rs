use super::super::types::{ReadBraceScan, ReadFoldSettings, ReadLineRange};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TreeSitterLanguage {
    Ts,
    Tsx,
    Js,
}
#[derive(Debug, Clone, Default)]
pub struct SyntaxNode {
    pub id: usize,
    pub kind: String,
    pub text: String,
    pub start_row: usize,
    pub end_row: usize,
    pub children: Vec<SyntaxNode>,
    pub has_error: bool,
    pub is_missing: bool,
    pub fields: std::collections::BTreeMap<String, usize>,
}
impl SyntaxNode {
    pub fn child_for_field_name(&self, field: &str) -> Option<&Self> {
        self.fields.get(field).and_then(|i| self.children.get(*i))
    }
    pub fn from_node(node: tree_sitter::Node<'_>, source: &str) -> Self {
        let mut children = Vec::new();
        let mut fields = std::collections::BTreeMap::new();
        for i in 0..node.child_count() {
            if let Some(child) = node.child(i) {
                if let Ok(index) = u32::try_from(i)
                    && let Some(field) = node.field_name_for_child(index)
                {
                    fields.insert(field.into(), children.len());
                }
                children.push(Self::from_node(child, source));
            }
        }
        Self {
            id: node.id(),
            kind: node.kind().into(),
            text: source[node.byte_range()].into(),
            start_row: node.start_position().row,
            end_row: node.end_position().row,
            children,
            has_error: node.has_error(),
            is_missing: node.is_missing(),
            fields,
        }
    }
}
const BODY_KINDS: &[&str] = &["statement_block", "object", "array", "switch_body"];
const FUNCTION_KINDS: &[&str] = &[
    "function_declaration",
    "function_expression",
    "function",
    "generator_function",
    "generator_function_declaration",
    "arrow_function",
    "method_definition",
    "function_signature",
    "method_signature",
    "abstract_method_signature",
];
const CLASS_KINDS: &[&str] = &["class", "class_declaration", "abstract_class_declaration"];
const OPAQUE_KINDS: &[&str] = &[
    "type_annotation",
    "type_arguments",
    "type_parameters",
    "type_alias_declaration",
    "interface_declaration",
    "type_predicate_annotation",
    "omitting_type_annotation",
    "opting_type_annotation",
    "asserts_annotation",
    "type_identifier",
    "predefined_type",
    "nested_type_identifier",
    "formal_parameters",
    "class_heritage",
    "extends_clause",
    "extends_type_clause",
    "implements_clause",
    "decorator",
    "import_statement",
    "object_pattern",
    "array_pattern",
    "computed_property_name",
    "ambient_declaration",
];
fn open_brace(node: &SyntaxNode) -> Option<&SyntaxNode> {
    node.children
        .iter()
        .find(|c| c.kind == "{" || c.kind == "[")
}
fn close_brace(node: &SyntaxNode) -> Option<&SyntaxNode> {
    node.children
        .iter()
        .rev()
        .find(|c| c.kind == "}" || c.kind == "]")
}
fn protect(guarded: &mut Vec<ReadLineRange>, start: usize, end: usize) {
    if end >= start {
        guarded.push(ReadLineRange {
            start_line: start + 1,
            end_line: end + 1,
        });
    }
}
fn visit(
    node: &SyntaxNode,
    settings: ReadFoldSettings,
    candidates: &mut Vec<ReadLineRange>,
    guarded: &mut Vec<ReadLineRange>,
) {
    let kind = node.kind.as_str();
    if OPAQUE_KINDS.contains(&kind)
        || kind.ends_with("_type")
        || kind.ends_with("_type_annotation")
        || (kind == "export_statement"
            && node
                .children
                .iter()
                .any(|c| ["export_clause", "*", "namespace_export"].contains(&c.kind.as_str())))
    {
        protect(guarded, node.start_row, node.end_row);
        return;
    }
    if kind == "comment" {
        if node.text.starts_with("/**") || node.text.starts_with("/*!") {
            protect(guarded, node.start_row, node.end_row);
        } else if node.end_row - node.start_row + 1 >= settings.min_comment_lines {
            candidates.push(ReadLineRange {
                start_line: node.start_row + 2,
                end_line: node.end_row,
            });
        }
        return;
    }
    if ["as_expression", "satisfies_expression"].contains(&kind)
        && let Some(operator) = node
            .children
            .iter()
            .find(|c| c.kind == "as" || c.kind == "satisfies")
    {
        protect(guarded, operator.start_row, node.end_row);
        for child in &node.children {
            if child.id == operator.id {
                break;
            }
            visit(child, settings, candidates, guarded);
        }
        return;
    }
    if ["assignment_expression", "for_in_statement"].contains(&kind)
        && let Some(left) = node.child_for_field_name("left")
    {
        let mut target = left;
        while target.kind == "parenthesized_expression" {
            let Some(inner) = target
                .children
                .iter()
                .find(|c| c.kind != "(" && c.kind != ")")
            else {
                break;
            };
            target = inner;
        }
        if target.kind == "object" || target.kind == "array" {
            protect(guarded, left.start_row, left.end_row);
            for child in &node.children {
                if child.id != left.id {
                    visit(child, settings, candidates, guarded);
                }
            }
            return;
        }
    }
    if FUNCTION_KINDS.contains(&kind) {
        match node.child_for_field_name("body") {
            Some(body) => {
                protect(guarded, node.start_row, body.start_row);
                visit(body, settings, candidates, guarded);
            }
            None => protect(guarded, node.start_row, node.end_row),
        }
        return;
    }
    if CLASS_KINDS.contains(&kind) {
        if let Some(body) = node.child_for_field_name("body")
            && let Some(open) = open_brace(body)
        {
            protect(guarded, node.start_row, open.start_row);
            for member in &body.children {
                visit(member, settings, candidates, guarded);
            }
        } else {
            protect(guarded, node.start_row, node.end_row);
        }
        return;
    }
    if BODY_KINDS.contains(&kind)
        && let (Some(open), Some(close)) = (open_brace(node), close_brace(node))
    {
        let start = open.start_row + 2;
        let end = close.start_row;
        if end >= start && end - start + 1 >= settings.min_body_lines {
            candidates.push(ReadLineRange {
                start_line: start,
                end_line: end,
            });
        }
    }
    for child in &node.children {
        visit(child, settings, candidates, guarded);
    }
}
pub fn fold_ranges_from_syntax(root: &SyntaxNode, settings: ReadFoldSettings) -> ReadBraceScan {
    if root.has_error || root.is_missing {
        return ReadBraceScan::ParseFailure {
            reason: "tree_sitter_parse_error".into(),
        };
    }
    let mut candidates = Vec::new();
    let mut guarded = Vec::new();
    visit(root, settings, &mut candidates, &mut guarded);
    ReadBraceScan::Parsed {
        ranges: candidates
            .into_iter()
            .filter(|r| {
                r.end_line >= r.start_line
                    && !guarded
                        .iter()
                        .any(|h| r.start_line <= h.end_line && r.end_line >= h.start_line)
            })
            .collect(),
    }
}
