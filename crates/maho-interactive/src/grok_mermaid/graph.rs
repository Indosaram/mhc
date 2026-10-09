//! Port of `grok-mermaid/src/graph.ts`.
//!
//! The shared diagram model. Flowchart, state, class and ER sources all parse
//! into a `Graph`; only sequence diagrams have their own model.

use std::collections::HashMap;

/// Caps that keep layout bounded; exceeding one drops the diagram to fallback.
pub const MAX_NODES: usize = 128;
pub const MAX_EDGES: usize = 512;
pub const MAX_GROUPS: usize = 24;
pub const MAX_GROUP_DEPTH: usize = 6;
/// Class members / ER attributes listed per box before eliding with `…`.
pub const MAX_MEMBERS: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    Rect,
    Round,
    Diamond,
}

/// Decoration at one end of an edge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Head {
    None,
    Arrow,
    Circle,
    Cross,
    Triangle,
    DiamondFill,
    DiamondOpen,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineKind {
    Solid,
    Dotted,
    Thick,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Dir {
    #[default]
    Down,
    Up,
    Right,
    Left,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Node {
    pub label: String,
    pub shape: Shape,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edge {
    pub from: usize,
    pub to: usize,
    pub label: Option<String>,
    pub head_to: Head,
    pub head_from: Head,
    pub line: LineKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Group {
    pub id: String,
    pub label: String,
    pub parent: Option<usize>,
}

/// Extra compartment content for class and ER boxes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ClassInfo {
    pub annotation: Option<String>,
    pub attrs: Vec<String>,
    pub methods: Vec<String>,
}

pub fn empty_class_info() -> ClassInfo {
    ClassInfo {
        annotation: None,
        attrs: Vec::new(),
        methods: Vec::new(),
    }
}

/// `LR`/`RL`/`BT` as written in a header or `direction` statement; else `down`.
pub fn parse_dir(token: &str) -> Dir {
    if token.eq_ignore_ascii_case("LR") {
        Dir::Right
    } else if token.eq_ignore_ascii_case("RL") {
        Dir::Left
    } else if token.eq_ignore_ascii_case("BT") {
        Dir::Up
    } else {
        Dir::Down
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Graph {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    pub index: HashMap<String, usize>,
    pub groups: Vec<Group>,
    /// Innermost subgraph each node was declared in, parallel to `nodes`.
    pub node_group: Vec<Option<usize>>,
    pub cur_group: Option<usize>,
    /// Set when a cap was hit; the caller abandons the parse.
    pub over_cap: bool,
    /// Text the flowchart grammar could not read and silently discarded.
    ///
    /// Flowchart parsing is deliberately lenient — a malformed statement
    /// contributes whatever prefix parsed and the rest is dropped — so without
    /// these the reader gets a clean diagram that is not what they wrote.
    pub warnings: Vec<String>,
    pub dir: Dir,
}

impl Graph {
    pub fn new(dir: Dir) -> Self {
        Self {
            nodes: Vec::new(),
            edges: Vec::new(),
            index: HashMap::new(),
            groups: Vec::new(),
            node_group: Vec::new(),
            cur_group: None,
            over_cap: false,
            warnings: Vec::new(),
            dir,
        }
    }

    /// Index of `id`, creating the node if new. A later declaration carrying a
    /// label overwrites the placeholder one an edge created. Returns `None` once
    /// `MAX_NODES` is reached, which aborts the parse.
    pub fn node_index(&mut self, id: &str, label: Option<&str>, shape: Shape) -> Option<usize> {
        if let Some(&existing) = self.index.get(id) {
            if let Some(lbl) = label {
                self.nodes[existing].label = lbl.to_owned();
                self.nodes[existing].shape = shape;
            }
            return Some(existing);
        }
        if self.nodes.len() >= MAX_NODES {
            self.over_cap = true;
            return None;
        }
        let idx = self.nodes.len();
        self.index.insert(id.to_owned(), idx);
        self.nodes.push(Node {
            label: label.unwrap_or(id).to_owned(),
            shape,
        });
        self.node_group.push(self.cur_group);
        Some(idx)
    }

    /// Set a node's label without disturbing its shape, creating it if new.
    pub fn node_label(&mut self, id: &str, label: &str) -> Option<usize> {
        if let Some(&existing) = self.index.get(id) {
            self.nodes[existing].label = label.to_owned();
            return Some(existing);
        }
        self.node_index(id, Some(label), Shape::Round)
    }

    /// Append an edge, or flag `over_cap` when `MAX_EDGES` is reached.
    pub fn push_edge(&mut self, edge: Edge) -> bool {
        if self.edges.len() >= MAX_EDGES {
            self.over_cap = true;
            return false;
        }
        self.edges.push(edge);
        true
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    #[test]
    fn test_constants() {
        assert_eq!(MAX_NODES, 128);
        assert_eq!(MAX_EDGES, 512);
        assert_eq!(MAX_GROUPS, 24);
        assert_eq!(MAX_GROUP_DEPTH, 6);
        assert_eq!(MAX_MEMBERS, 8);
    }

    #[test]
    fn test_empty_class_info() {
        let info = empty_class_info();
        assert_eq!(info.annotation, None);
        assert!(info.attrs.is_empty());
        assert!(info.methods.is_empty());
    }

    #[test]
    fn test_parse_dir() {
        assert_eq!(parse_dir("LR"), Dir::Right);
        assert_eq!(parse_dir("lr"), Dir::Right);
        assert_eq!(parse_dir("RL"), Dir::Left);
        assert_eq!(parse_dir("rl"), Dir::Left);
        assert_eq!(parse_dir("BT"), Dir::Up);
        assert_eq!(parse_dir("bt"), Dir::Up);
        assert_eq!(parse_dir("TD"), Dir::Down);
        assert_eq!(parse_dir("TB"), Dir::Down);
        assert_eq!(parse_dir("unknown"), Dir::Down);
        assert_eq!(parse_dir(""), Dir::Down);
    }

    #[test]
    fn test_graph_new() {
        let g = Graph::new(Dir::Right);
        assert_eq!(g.dir, Dir::Right);
        assert!(g.nodes.is_empty());
        assert!(g.edges.is_empty());
        assert!(g.index.is_empty());
        assert!(g.groups.is_empty());
        assert!(g.node_group.is_empty());
        assert_eq!(g.cur_group, None);
        assert!(!g.over_cap);
        assert!(g.warnings.is_empty());
    }

    #[test]
    fn test_graph_node_index_creation_and_update() {
        let mut g = Graph {
            cur_group: Some(2),
            ..Graph::default()
        };

        // Create new node with default label = id
        let idx_a = g.node_index("A", None, Shape::Rect);
        assert_eq!(idx_a, Some(0));
        assert_eq!(g.nodes[0].label, "A");
        assert_eq!(g.nodes[0].shape, Shape::Rect);
        assert_eq!(g.node_group[0], Some(2));
        assert_eq!(g.index.get("A"), Some(&0));

        // Create new node with explicit label
        let idx_b = g.node_index("B", Some("Custom B"), Shape::Diamond);
        assert_eq!(idx_b, Some(1));
        assert_eq!(g.nodes[1].label, "Custom B");
        assert_eq!(g.nodes[1].shape, Shape::Diamond);
        assert_eq!(g.node_group[1], Some(2));

        // Existing node re-referenced with label=None does not change existing label or shape
        let idx_a_again = g.node_index("A", None, Shape::Diamond);
        assert_eq!(idx_a_again, Some(0));
        assert_eq!(g.nodes[0].label, "A");
        assert_eq!(g.nodes[0].shape, Shape::Rect);

        // Existing node updated with new label and shape
        let idx_a_update = g.node_index("A", Some("Updated A"), Shape::Round);
        assert_eq!(idx_a_update, Some(0));
        assert_eq!(g.nodes[0].label, "Updated A");
        assert_eq!(g.nodes[0].shape, Shape::Round);
    }

    #[test]
    fn test_graph_node_index_over_cap() {
        let mut g = Graph::default();
        for i in 0..MAX_NODES {
            let id = format!("node_{i}");
            assert_eq!(g.node_index(&id, None, Shape::Rect), Some(i));
        }
        assert!(!g.over_cap);

        // Attempting to add one more node should hit cap
        let overflow = g.node_index("overflow", None, Shape::Rect);
        assert_eq!(overflow, None);
        assert!(g.over_cap);

        // Existing node can still be looked up even when overCap is true
        let existing = g.node_index("node_0", None, Shape::Rect);
        assert_eq!(existing, Some(0));
    }

    #[test]
    fn test_graph_node_label() {
        let mut g = Graph::default();

        // Node does not exist: creates with Shape::Round and given label
        let idx_x = g.node_label("X", "Node X Label");
        assert_eq!(idx_x, Some(0));
        assert_eq!(g.nodes[0].label, "Node X Label");
        assert_eq!(g.nodes[0].shape, Shape::Round);

        // Create node Y with Shape::Diamond via node_index
        g.node_index("Y", Some("Initial Y"), Shape::Diamond);
        assert_eq!(g.nodes[1].shape, Shape::Diamond);

        // Update node Y with node_label: label changes, shape is preserved
        let idx_y = g.node_label("Y", "Updated Y Label");
        assert_eq!(idx_y, Some(1));
        assert_eq!(g.nodes[1].label, "Updated Y Label");
        assert_eq!(g.nodes[1].shape, Shape::Diamond);
    }

    #[test]
    fn test_graph_push_edge() {
        let mut g = Graph::default();
        let e1 = Edge {
            from: 0,
            to: 1,
            label: Some("links to".to_string()),
            head_to: Head::Arrow,
            head_from: Head::None,
            line: LineKind::Solid,
        };
        assert!(g.push_edge(e1.clone()));
        assert_eq!(g.edges.len(), 1);
        assert_eq!(g.edges[0], e1);
        assert!(!g.over_cap);

        // Fill up to MAX_EDGES
        for _ in 1..MAX_EDGES {
            assert!(g.push_edge(e1.clone()));
        }
        assert_eq!(g.edges.len(), MAX_EDGES);
        assert!(!g.over_cap);

        // Overflow edge
        assert!(!g.push_edge(e1));
        assert_eq!(g.edges.len(), MAX_EDGES);
        assert!(g.over_cap);
    }
}
