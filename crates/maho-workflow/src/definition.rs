//! The workflow definition (spec §1): `{key, name, nodes[]}`, per-node target checks, graph
//! validation (§1.1, every error collected) and the canonical fingerprint used for key reuse.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// One node: a delegated child task.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowNode {
    pub id: String,
    pub prompt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_summary: Option<String>,
    #[serde(rename = "dependsOn", default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub load_skills: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

impl WorkflowNode {
    /// `category:<name>` or `agent:<name>`.
    pub fn target_label(&self) -> String {
        match (&self.category, &self.subagent_type) {
            (Some(category), _) => format!("category:{category}"),
            (None, Some(agent)) => format!("agent:{agent}"),
            (None, None) => "-".to_owned(),
        }
    }

    /// The label shown to users (label, else id).
    pub fn display_label(&self) -> &str {
        self.label
            .as_deref()
            .filter(|label| !label.trim().is_empty())
            .unwrap_or(&self.id)
    }
}

/// A workflow definition.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowDefinition {
    pub key: String,
    pub name: String,
    #[serde(default)]
    pub nodes: Vec<WorkflowNode>,
}

/// One validation problem.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationIssue {
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    /// For `cycle`: the cycle path, first node repeated at the end.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub path: Vec<String>,
}

impl ValidationIssue {
    fn new(code: &str, message: String, node_id: Option<&str>) -> Self {
        Self {
            code: code.to_owned(),
            message,
            node_id: node_id.map(str::to_owned),
            path: Vec::new(),
        }
    }
}

/// Code of a target error (both/neither of `category`/`subagent_type`, `model` with
/// `category`). The spec names no code for these; this one is mhc's.
pub const INVALID_TARGET: &str = "invalid_target";

/// Limits that validation enforces.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub max_nodes: usize,
    pub max_prompt_bytes: usize,
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id != "."
        && id != ".."
        && !id.chars().any(|c| matches!(c, '/' | '\\' | ':' | '\0'))
}

fn blank(value: Option<&String>) -> bool {
    value.is_none_or(|text| text.trim().is_empty())
}

/// Per-node target errors (reported before graph validation).
fn target_issues(nodes: &[WorkflowNode]) -> Vec<ValidationIssue> {
    let mut issues = Vec::new();
    for node in nodes {
        let category = !blank(node.category.as_ref());
        let agent = !blank(node.subagent_type.as_ref());
        let id = Some(node.id.as_str());
        match (category, agent) {
            (true, true) => issues.push(ValidationIssue::new(
                INVALID_TARGET,
                format!("node \"{}\" names both a category and a subagent_type; pick one", node.id),
                id,
            )),
            (false, false) => issues.push(ValidationIssue::new(
                INVALID_TARGET,
                format!("node \"{}\" needs a category or a subagent_type", node.id),
                id,
            )),
            _ => {}
        }
        if category && !blank(node.model.as_ref()) {
            issues.push(ValidationIssue::new(
                INVALID_TARGET,
                format!(
                    "node \"{}\": `model` is only accepted together with `subagent_type` (the category picks the model)",
                    node.id
                ),
                id,
            ));
        }
    }
    issues
}

/// Finds one dependency cycle among known, non-self edges; returns its path with the first node
/// repeated at the end. Deterministic: nodes and edges are visited in id order.
fn find_cycle(nodes: &[WorkflowNode]) -> Option<Vec<String>> {
    let mut graph: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for node in nodes {
        graph.entry(node.id.as_str()).or_default();
    }
    for node in nodes {
        for dep in &node.depends_on {
            if dep != &node.id && graph.contains_key(dep.as_str()) {
                graph.entry(node.id.as_str()).or_default().insert(dep.as_str());
            }
        }
    }
    // 0 = unvisited, 1 = on stack, 2 = done.
    let mut mark: HashMap<&str, u8> = HashMap::new();
    let mut stack: Vec<&str> = Vec::new();
    fn visit<'a>(
        id: &'a str,
        graph: &BTreeMap<&'a str, BTreeSet<&'a str>>,
        mark: &mut HashMap<&'a str, u8>,
        stack: &mut Vec<&'a str>,
    ) -> Option<Vec<String>> {
        mark.insert(id, 1);
        stack.push(id);
        for next in graph.get(id).into_iter().flatten() {
            match mark.get(next).copied().unwrap_or(0) {
                1 => {
                    let start = stack.iter().position(|item| item == next).unwrap_or(0);
                    let mut path: Vec<String> =
                        stack[start..].iter().map(|item| (*item).to_owned()).collect();
                    path.push((*next).to_owned());
                    return Some(path);
                }
                0 => {
                    if let Some(path) = visit(next, graph, mark, stack) {
                        return Some(path);
                    }
                }
                _ => {}
            }
        }
        stack.pop();
        mark.insert(id, 2);
        None
    }
    let ids: Vec<&str> = graph.keys().copied().collect();
    for id in ids {
        if mark.get(id).copied().unwrap_or(0) == 0
            && let Some(path) = visit(id, &graph, &mut mark, &mut stack)
        {
            return Some(path);
        }
    }
    None
}

/// Validates `definition`; an empty result means it is valid. Target errors come first, then the
/// §1.1 graph rules in table order.
pub fn validate(definition: &WorkflowDefinition, limits: Limits) -> Vec<ValidationIssue> {
    let nodes = &definition.nodes;
    let mut issues = target_issues(nodes);
    if nodes.is_empty() {
        issues.push(ValidationIssue::new(
            "empty_graph",
            "the workflow has no nodes".to_owned(),
            None,
        ));
        return issues;
    }
    if nodes.len() > limits.max_nodes {
        issues.push(ValidationIssue::new(
            "node_count_exceeded",
            format!(
                "the workflow has {} nodes; at most {} are allowed (task.dag.max_nodes_per_run)",
                nodes.len(),
                limits.max_nodes
            ),
            None,
        ));
    }
    for node in nodes {
        if !valid_id(&node.id) {
            issues.push(ValidationIssue::new(
                "invalid_node_id",
                format!(
                    "node id \"{}\" is not allowed (must be non-empty, not `.` or `..`, without `/`, `\\`, `:` or NUL)",
                    node.id.escape_default()
                ),
                Some(node.id.as_str()),
            ));
        }
    }
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for node in nodes {
        *counts.entry(node.id.as_str()).or_default() += 1;
    }
    let mut reported = BTreeSet::new();
    for node in nodes {
        if counts.get(node.id.as_str()).copied().unwrap_or(0) > 1 && reported.insert(node.id.as_str()) {
            issues.push(ValidationIssue::new(
                "duplicate_node_id",
                format!("node id \"{}\" is used more than once", node.id),
                Some(node.id.as_str()),
            ));
        }
    }
    for node in nodes {
        if node.prompt.len() > limits.max_prompt_bytes {
            issues.push(ValidationIssue::new(
                "prompt_bytes_exceeded",
                format!(
                    "node \"{}\" has a {}-byte prompt; at most {} bytes are allowed (task.dag.max_prompt_bytes)",
                    node.id,
                    node.prompt.len(),
                    limits.max_prompt_bytes
                ),
                Some(node.id.as_str()),
            ));
        }
    }
    for node in nodes {
        if node.depends_on.len() > limits.max_nodes {
            issues.push(ValidationIssue::new(
                "dependency_fanout_exceeded",
                format!(
                    "node \"{}\" lists {} dependencies; at most {} are allowed",
                    node.id,
                    node.depends_on.len(),
                    limits.max_nodes
                ),
                Some(node.id.as_str()),
            ));
        }
    }
    for node in nodes {
        if node.depends_on.iter().any(|dep| dep == &node.id) {
            issues.push(ValidationIssue::new(
                "self_dependency",
                format!("node \"{}\" depends on itself", node.id),
                Some(node.id.as_str()),
            ));
        }
    }
    for node in nodes {
        let mut seen = BTreeSet::new();
        for dep in &node.depends_on {
            if !counts.contains_key(dep.as_str()) && seen.insert(dep.as_str()) {
                issues.push(ValidationIssue::new(
                    "unknown_dependency",
                    format!("node \"{}\" depends on \"{dep}\", which is not in the workflow", node.id),
                    Some(node.id.as_str()),
                ));
            }
        }
    }
    if let Some(path) = find_cycle(nodes) {
        let mut issue = ValidationIssue::new(
            "cycle",
            format!("dependency cycle: {}", path.join(" -> ")),
            path.first().map(String::as_str),
        );
        issue.path = path;
        issues.push(issue);
    }
    issues
}

/// Canonical fingerprint: SHA-256 over the definition with nodes ordered by id and each node's
/// dependencies and skills sorted and deduplicated, so equivalent definitions match.
pub fn fingerprint(definition: &WorkflowDefinition) -> String {
    let mut canonical = definition.clone();
    for node in &mut canonical.nodes {
        node.depends_on.sort();
        node.depends_on.dedup();
    }
    canonical.nodes.sort_by(|a, b| a.id.cmp(&b.id));
    let bytes = serde_json::to_vec(&canonical).unwrap_or_default();
    hex::encode(Sha256::digest(bytes))
}

/// Fingerprint of one node (used by `amend` to tell identical nodes from modified ones).
pub fn node_fingerprint(node: &WorkflowNode) -> String {
    let mut canonical = node.clone();
    canonical.depends_on.sort();
    canonical.depends_on.dedup();
    let bytes = serde_json::to_vec(&canonical).unwrap_or_default();
    hex::encode(Sha256::digest(bytes))
}

#[cfg(test)]
pub(crate) fn node(id: &str, deps: &[&str]) -> WorkflowNode {
    WorkflowNode {
        id: id.to_owned(),
        prompt: format!("do {id}"),
        depends_on: deps.iter().map(|d| (*d).to_owned()).collect(),
        category: Some("quick".to_owned()),
        ..WorkflowNode::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMITS: Limits = Limits {
        max_nodes: 4,
        max_prompt_bytes: 10,
    };

    fn def(nodes: Vec<WorkflowNode>) -> WorkflowDefinition {
        WorkflowDefinition {
            key: "k".into(),
            name: "n".into(),
            nodes,
        }
    }

    fn codes(issues: &[ValidationIssue]) -> Vec<&str> {
        issues.iter().map(|issue| issue.code.as_str()).collect()
    }

    /// Acceptance test 3: each §1.1 rule yields its code.
    #[test]
    fn acceptance_03_each_rule_yields_its_code() {
        assert_eq!(codes(&validate(&def(vec![]), LIMITS)), ["empty_graph"]);
        let many = (0..5).map(|i| node(&format!("n{i}"), &[])).collect();
        assert_eq!(codes(&validate(&def(many), LIMITS)), ["node_count_exceeded"]);
        for bad in ["", ".", "..", "a/b", "a\\b", "a:b", "a\0b"] {
            assert_eq!(
                codes(&validate(&def(vec![node(bad, &[])]), LIMITS)),
                ["invalid_node_id"],
                "{bad:?}"
            );
        }
        let dup = def(vec![node("a", &[]), node("a", &[]), node("a", &[])]);
        assert_eq!(codes(&validate(&dup, LIMITS)), ["duplicate_node_id"]);
        let mut long = node("a", &[]);
        long.prompt = "x".repeat(11);
        assert_eq!(codes(&validate(&def(vec![long]), LIMITS)), ["prompt_bytes_exceeded"]);
        let fan = def(vec![
            node("a", &[]),
            node("b", &["a", "a", "a", "a", "a"]),
        ]);
        assert_eq!(codes(&validate(&fan, LIMITS)), ["dependency_fanout_exceeded"]);
        assert_eq!(
            codes(&validate(&def(vec![node("a", &["a"])]), LIMITS)),
            ["self_dependency"]
        );
        assert_eq!(
            codes(&validate(&def(vec![node("a", &["zz"])]), LIMITS)),
            ["unknown_dependency"]
        );
        let cyc = def(vec![node("a", &["b"]), node("b", &["a"])]);
        assert_eq!(codes(&validate(&cyc, LIMITS)), ["cycle"]);
    }

    /// Acceptance test 3 (second half, validation side): two problems are both reported.
    #[test]
    fn acceptance_03_two_problems_reported_together() {
        let d = def(vec![node("a", &["a"]), node("b", &["missing"])]);
        assert_eq!(
            codes(&validate(&d, LIMITS)),
            ["self_dependency", "unknown_dependency"]
        );
    }

    /// Acceptance test 4: `x -> y -> x` reports a cycle path that starts and ends at one node.
    #[test]
    fn acceptance_04_cycle_path() {
        let d = def(vec![node("x", &["y"]), node("y", &["x"])]);
        let issues = validate(&d, LIMITS);
        assert_eq!(issues.len(), 1);
        let path = &issues[0].path;
        assert_eq!(path.len(), 3);
        assert_eq!(path.first(), path.last());
        assert!(issues[0].message.contains("->"));
    }

    /// Acceptance test 5: both targets, or `model` with `category`, is rejected.
    #[test]
    fn acceptance_05_target_errors() {
        let mut both = node("a", &[]);
        both.subagent_type = Some("task".into());
        assert_eq!(codes(&validate(&def(vec![both]), LIMITS)), [INVALID_TARGET]);
        let mut model = node("a", &[]);
        model.model = Some("p/m".into());
        assert_eq!(codes(&validate(&def(vec![model]), LIMITS)), [INVALID_TARGET]);
        let mut neither = node("a", &[]);
        neither.category = None;
        assert_eq!(codes(&validate(&def(vec![neither]), LIMITS)), [INVALID_TARGET]);
        let mut agent = node("a", &[]);
        agent.category = None;
        agent.subagent_type = Some("task".into());
        agent.model = Some("p/m".into());
        assert!(validate(&def(vec![agent]), LIMITS).is_empty());
    }

    #[test]
    fn fingerprint_ignores_node_and_dependency_order() {
        let a = def(vec![node("a", &[]), node("b", &["a"]), node("c", &["a", "b"])]);
        let b = def(vec![node("c", &["b", "a"]), node("a", &[]), node("b", &["a"])]);
        assert_eq!(fingerprint(&a), fingerprint(&b));
        let mut changed = a.clone();
        changed.nodes[0].prompt = "other".into();
        assert_ne!(fingerprint(&a), fingerprint(&changed));
    }

    #[test]
    fn parses_tool_json() {
        let json = serde_json::json!({"key": "k", "name": "n", "nodes": [
            {"id": "a", "prompt": "p", "dependsOn": ["b"], "load_skills": ["s"], "subagent_type": "task", "model": "x/y"}
        ]});
        let parsed: WorkflowDefinition = serde_json::from_value(json).expect("parse");
        assert_eq!(parsed.nodes[0].depends_on, ["b"]);
        assert_eq!(parsed.nodes[0].target_label(), "agent:task");
    }
}
