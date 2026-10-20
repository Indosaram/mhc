use super::{
    apply_immutable_trusted::apply_immutable_trusted, diff::emit_string, DeltaError, JsonValue, Op,
    Path, Result, Seg,
};
use crate::json::{copy_json, CopyJsonOptions};
use array_overlay::ArrayOverlay;

mod array_overlay {
    use super::{node, ArrayEntry, NodeRef};
    use crate::types::JsonValue;
    use std::{
        cell::RefCell,
        collections::{BTreeMap, HashMap},
        rc::Rc,
        sync::Arc,
    };

    struct Source {
        values: Arc<Vec<JsonValue>>,
        base: bool,
        nodes: RefCell<BTreeMap<usize, NodeRef>>,
    }
    #[derive(Clone)]
    struct Piece {
        source: Rc<Source>,
        start: usize,
        length: usize,
        reversed: bool,
    }
    impl Piece {
        fn index(&self, offset: usize) -> usize {
            if self.reversed {
                self.start - offset
            } else {
                self.start + offset
            }
        }
        fn entry(&self, offset: usize) -> ArrayEntry {
            let index = self.index(offset);
            let reference = self
                .source
                .nodes
                .borrow_mut()
                .entry(index)
                .or_insert_with(|| node(self.source.values[index].clone()))
                .clone();
            ArrayEntry {
                node: reference,
                origin: self.source.base.then_some(index),
            }
        }
    }
    struct PieceNode {
        piece: Piece,
        left: Tree,
        right: Tree,
        priority: u32,
        elements: usize,
    }
    type Tree = Option<Box<PieceNode>>;
    struct SourceLocations {
        source: Rc<Source>,
        ranges: Vec<(usize, Piece)>,
    }
    fn length(tree: &Tree) -> usize {
        tree.as_ref().map_or(0, |n| n.elements)
    }
    fn update(node: &mut PieceNode) {
        node.elements = length(&node.left) + node.piece.length + length(&node.right);
    }
    fn merge(left: Tree, right: Tree) -> Tree {
        match (left, right) {
            (None, right) => right,
            (left, None) => left,
            (Some(mut left), Some(mut right)) => {
                if left.priority >= right.priority {
                    left.right = merge(left.right.take(), Some(right));
                    update(&mut left);
                    Some(left)
                } else {
                    right.left = merge(Some(left), right.left.take());
                    update(&mut right);
                    Some(right)
                }
            }
        }
    }
    fn create(seed: &mut u32, piece: Piece) -> Box<PieceNode> {
        *seed ^= *seed << 13;
        *seed ^= *seed >> 17;
        *seed ^= *seed << 5;
        Box::new(PieceNode {
            elements: piece.length,
            piece,
            left: None,
            right: None,
            priority: *seed,
        })
    }
    fn split(seed: &mut u32, tree: Tree, index: usize) -> (Tree, Tree) {
        let Some(mut root) = tree else {
            return (None, None);
        };
        let left_length = length(&root.left);
        let end = left_length + root.piece.length;
        if index < left_length {
            let (left, right) = split(seed, root.left.take(), index);
            root.left = right;
            update(&mut root);
            return (left, Some(root));
        }
        if index > end {
            let (left, right) = split(seed, root.right.take(), index - end);
            root.right = left;
            update(&mut root);
            return (Some(root), right);
        }
        if index == left_length {
            let left = root.left.take();
            update(&mut root);
            return (left, Some(root));
        }
        if index == end {
            let right = root.right.take();
            update(&mut root);
            return (Some(root), right);
        }
        let offset = index - left_length;
        let mut first = root.piece.clone();
        first.length = offset;
        let mut second = root.piece;
        second.start = second.index(offset);
        second.length -= offset;
        (
            merge(root.left, Some(create(seed, first))),
            merge(Some(create(seed, second)), root.right),
        )
    }
    fn join(seed: &mut u32, left: Tree, right: Tree) -> Tree {
        let (Some(left_node), Some(right_node)) = (&left, &right) else {
            return merge(left, right);
        };
        let mut last = left_node.as_ref();
        while let Some(next) = &last.right {
            last = next;
        }
        let mut first = right_node.as_ref();
        while let Some(next) = &first.left {
            first = next;
        }
        let a = last.piece.clone();
        let b = first.piece.clone();
        let contiguous = if a.reversed {
            a.start.checked_sub(a.length) == Some(b.start)
        } else {
            a.start.checked_add(a.length) == Some(b.start)
        };
        if !Rc::ptr_eq(&a.source, &b.source) || a.reversed != b.reversed || !contiguous {
            return merge(left, right);
        }
        let left_length = length(&left);
        let (rest, _) = split(seed, left, left_length - a.length);
        let (_, tail) = split(seed, right, b.length);
        let combined = Piece {
            length: a.length + b.length,
            ..a
        };
        let middle = Some(create(seed, combined));
        let head = join(seed, rest, middle);
        join(seed, head, tail)
    }
    fn pieces(tree: &Tree, output: &mut Vec<Piece>) {
        if let Some(node) = tree {
            pieces(&node.left, output);
            output.push(node.piece.clone());
            pieces(&node.right, output);
        }
    }

    pub(super) struct ArrayOverlay {
        root: Tree,
        seed: u32,
        pub structural: bool,
        locations: RefCell<Option<Vec<SourceLocations>>>,
    }
    impl ArrayOverlay {
        pub fn values(&self) -> Vec<JsonValue> {
            let mut all = Vec::new();
            pieces(&self.root, &mut all);
            let mut output = Vec::with_capacity(self.len());
            for piece in all {
                let nodes = piece.source.nodes.borrow();
                for offset in 0..piece.length {
                    let index = piece.index(offset);
                    output.push(match nodes.get(&index) {
                        Some(node) => super::materialize(node),
                        None => piece.source.values[index].clone(),
                    });
                }
            }
            output
        }
        pub fn origins(&self) -> Vec<Option<usize>> {
            let mut all = Vec::new();
            pieces(&self.root, &mut all);
            all.iter()
                .flat_map(|p| (0..p.length).map(move |i| p.source.base.then(|| p.index(i))))
                .collect()
        }
        pub fn new(base: Arc<Vec<JsonValue>>) -> Self {
            let mut result = Self {
                root: None,
                seed: 0x9e3779b9,
                structural: false,
                locations: RefCell::new(None),
            };
            if !base.is_empty() {
                let piece = Piece {
                    length: base.len(),
                    source: Rc::new(Source {
                        values: base,
                        base: true,
                        nodes: RefCell::new(BTreeMap::new()),
                    }),
                    start: 0,
                    reversed: false,
                };
                result.root = Some(create(&mut result.seed, piece));
            }
            result
        }
        pub fn len(&self) -> usize {
            length(&self.root)
        }
        pub fn get(&self, mut index: usize) -> Option<ArrayEntry> {
            let mut tree = self.root.as_ref();
            while let Some(node) = tree {
                let left = length(&node.left);
                if index < left {
                    tree = node.left.as_ref();
                } else if index >= left + node.piece.length {
                    index -= left + node.piece.length;
                    tree = node.right.as_ref();
                } else {
                    return Some(node.piece.entry(index - left));
                }
            }
            None
        }
        pub fn iter(&self) -> impl Iterator<Item = ArrayEntry> + '_ {
            (0..self.len()).filter_map(|i| self.get(i))
        }
        pub fn accessed(&self) -> Vec<(usize, ArrayEntry)> {
            if self.locations.borrow().is_none() {
                let mut all = Vec::new();
                pieces(&self.root, &mut all);
                let mut logical = 0;
                let mut sources: HashMap<_, SourceLocations> = HashMap::new();
                for piece in all {
                    let source = sources.entry(Rc::as_ptr(&piece.source)).or_insert_with(|| SourceLocations {
                        source: piece.source.clone(), ranges: Vec::new(),
                    });
                    source.ranges.push((logical, piece.clone()));
                    logical += piece.length;
                }
                let mut sources: Vec<_> = sources.into_values().collect();
                for source in &mut sources {
                    source.ranges.sort_by_key(|(_, piece)| piece.start.min(piece.index(piece.length - 1)));
                }
                *self.locations.borrow_mut() = Some(sources);
            }
            let mut output = Vec::new();
            let locations = self.locations.borrow();
            for source in locations.iter().flatten() {
                for (&index, node) in source.source.nodes.borrow().iter() {
                    let at = source.ranges.partition_point(|(_, piece)| {
                        piece.start.min(piece.index(piece.length - 1)) <= index
                    });
                    let Some((logical, piece)) = at.checked_sub(1).and_then(|at| source.ranges.get(at)) else {
                        continue;
                    };
                    let offset = if piece.reversed {
                        piece.start.checked_sub(index)
                    } else {
                        index.checked_sub(piece.start)
                    };
                    if let Some(offset) = offset.filter(|i| *i < piece.length) {
                        output.push((
                            *logical + offset,
                            ArrayEntry {
                                node: node.clone(),
                                origin: piece.source.base.then_some(index),
                            },
                        ));
                    }
                }
            }
            output.sort_by_key(|(index, _)| *index);
            output
        }
        pub fn set(&mut self, index: usize, value: NodeRef) {
            if index == self.len() {
                self.splice(
                    index,
                    index,
                    vec![ArrayEntry {
                        node: value,
                        origin: None,
                    }],
                );
                return;
            }
            let mut at = index;
            let mut tree = self.root.as_mut();
            while let Some(node) = tree {
                let left = length(&node.left);
                if at < left {
                    tree = node.left.as_mut();
                } else if at >= left + node.piece.length {
                    at -= left + node.piece.length;
                    tree = node.right.as_mut();
                } else {
                    node.piece
                        .source
                        .nodes
                        .borrow_mut()
                        .insert(node.piece.index(at - left), value);
                    return;
                }
            }
        }
        pub fn splice(
            &mut self,
            start: usize,
            end: usize,
            inserted: Vec<ArrayEntry>,
        ) -> Vec<ArrayEntry> {
            self.locations.get_mut().take();
            let (left, rest) = split(&mut self.seed, self.root.take(), start);
            let (removed, right) = split(&mut self.seed, rest, end - start);
            let mut removed_pieces = Vec::new();
            pieces(&removed, &mut removed_pieces);
            let output = removed_pieces
                .iter()
                .flat_map(|p| (0..p.length).map(move |i| p.entry(i)))
                .collect();
            let middle = if inserted.is_empty() {
                None
            } else {
                let length = inserted.len();
                let values = Arc::new(
                    inserted
                        .iter()
                        .map(|e| e.node.borrow().base.clone())
                        .collect(),
                );
                let nodes = inserted
                    .into_iter()
                    .enumerate()
                    .map(|(i, e)| (i, e.node))
                    .collect();
                Some(create(
                    &mut self.seed,
                    Piece {
                        source: Rc::new(Source {
                            values,
                            base: false,
                            nodes: RefCell::new(nodes),
                        }),
                        start: 0,
                        length,
                        reversed: false,
                    },
                ))
            };
            let head = join(&mut self.seed, left, middle);
            self.root = join(&mut self.seed, head, right);
            self.structural = true;
            output
        }
        pub fn reverse(&mut self) {
            self.locations.get_mut().take();
            fn reverse(tree: &mut Tree) {
                if let Some(node) = tree {
                    std::mem::swap(&mut node.left, &mut node.right);
                    node.piece.start = node.piece.index(node.piece.length - 1);
                    node.piece.reversed = !node.piece.reversed;
                    reverse(&mut node.left);
                    reverse(&mut node.right);
                }
            }
            reverse(&mut self.root);
            self.structural = true;
        }
        pub fn reorder(&mut self, entries: Vec<ArrayEntry>) {
            self.locations.get_mut().take();
            let base = self
                .iter()
                .filter_map(|e| e.origin.map(|i| (i, e)))
                .collect::<HashMap<_, _>>();
            let mut sources = Vec::new();
            pieces(&self.root, &mut sources);
            let base_source = sources
                .iter()
                .find(|p| p.source.base)
                .map(|p| p.source.clone());
            self.root = None;
            for entry in entries {
                let piece = match (entry.origin, &base_source) {
                    (Some(index), Some(source)) if base.contains_key(&index) => Piece {
                        source: {
                            source.nodes.borrow_mut().insert(index, entry.node.clone());
                            source.clone()
                        },
                        start: index,
                        length: 1,
                        reversed: false,
                    },
                    _ => Piece {
                        source: Rc::new(Source {
                            values: Arc::new(vec![entry.node.borrow().base.clone()]),
                            base: false,
                            nodes: RefCell::new(BTreeMap::from([(0, entry.node.clone())])),
                        }),
                        start: 0,
                        length: 1,
                        reversed: false,
                    },
                };
                self.root = merge(self.root.take(), Some(create(&mut self.seed, piece)));
            }
            self.locations.get_mut().take();
            self.structural = true;
        }
    }
}
use std::{
    cell::{Cell, RefCell},
    collections::HashSet,
    rc::{Rc, Weak},
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Status {
    Open,
    Prepared,
    Consumed,
    Aborted,
    Stale,
}
struct Authority {
    revision: Cell<u64>,
    contexts: RefCell<Vec<Weak<Overlay>>>,
}
struct Overlay {
    authority: Weak<Authority>,
    revision: u64,
    status: Cell<Status>,
    dirty: RefCell<Vec<Weak<RefCell<Node>>>>,
    nodes: RefCell<Vec<Weak<RefCell<Node>>>>,
}
impl Overlay {
    fn release(&self) {
        self.dirty.borrow_mut().clear();
        for reference in self
            .nodes
            .borrow_mut()
            .drain(..)
            .filter_map(|n| n.upgrade())
        {
            let mut node = reference.borrow_mut();
            node.base = JsonValue::Null;
            node.contents = Contents::Unread;
            node.writes.clear();
            node.deletes.clear();
            node.readded.clear();
        }
    }
    fn writable(&self) -> Result<()> {
        self.readable()?;
        if self.status.get() != Status::Open {
            return Err(DeltaError::Settled);
        }
        Ok(())
    }
    fn readable(&self) -> Result<()> {
        if self
            .authority
            .upgrade()
            .is_some_and(|a| a.revision.get() != self.revision)
        {
            return Err(DeltaError::Settled);
        }
        match self.status.get() {
            Status::Open => Ok(()),
            Status::Prepared | Status::Consumed | Status::Aborted | Status::Stale => {
                Err(DeltaError::Settled)
            }
        }
    }
}
pub(super) type NodeRef = Rc<RefCell<Node>>;
struct Entry {
    key: crate::types::JsString,
    node: NodeRef,
    original: bool,
}
struct ObjectOverlay {
    base: std::sync::Arc<crate::types::JsonObject>,
    entries: Vec<Entry>,
    deleted: HashSet<crate::types::JsString>,
}
impl ObjectOverlay {
    fn get(&mut self, key: &crate::types::JsString) -> Option<NodeRef> {
        if self.deleted.contains(key) { return None; }
        if let Some(entry) = self.entries.iter().find(|entry| &entry.key == key) {
            return Some(entry.node.clone());
        }
        let (_, value) = self.base.iter().find(|(base_key, _)| base_key == key)?;
        let node = node(value.clone());
        self.entries.push(Entry { key: key.clone(), node: node.clone(), original: true });
        Some(node)
    }
    fn keys(&self, readded: &[crate::types::JsString]) -> Vec<crate::types::JsString> {
        let mut keys: Vec<_> = self.base.iter()
            .filter(|(key, _)| !self.deleted.contains(key))
            .map(|(key, _)| key.clone()).collect();
        for entry in &self.entries {
            if !keys.contains(&entry.key) { keys.push(entry.key.clone()); }
        }
        keys.retain(|key| !readded.contains(key));
        keys.extend(readded.iter().cloned());
        keys.sort_by(|a, b| match (a.array_index(), b.array_index()) {
            (Some(a), Some(b)) => a.cmp(&b),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        });
        keys
    }
    fn values(&self, readded: &[crate::types::JsString]) -> crate::types::JsonObject {
        self.keys(readded).into_iter().filter_map(|key| {
            let value = self.entries.iter().find(|entry| entry.key == key)
                .map(|entry| materialize(&entry.node))
                .or_else(|| self.base.iter().find(|(k, _)| k == &key).map(|(_, v)| v.clone()))?;
            Some((key, value))
        }).collect()
    }
}
#[derive(Clone)]
pub(super) struct ArrayEntry {
    pub node: NodeRef,
    pub origin: Option<usize>,
}
enum Contents {
    Unread,
    Primitive,
    Object(ObjectOverlay),
    Array(ArrayOverlay),
}
pub(super) struct Node {
    pub base: JsonValue,
    contents: Contents,
    writes: Vec<crate::types::JsString>,
    deletes: Vec<crate::types::JsString>,
    readded: Vec<crate::types::JsString>,
    dirty: bool,
    subtree_dirty: bool,
    dirty_order: usize,
}
pub(super) fn node(value: JsonValue) -> NodeRef {
    Rc::new(RefCell::new(Node {
        base: value,
        contents: Contents::Unread,
        writes: Vec::new(),
        deletes: Vec::new(),
        readded: Vec::new(),
        dirty: false,
        subtree_dirty: false,
        dirty_order: usize::MAX,
    }))
}
fn load(node: &mut Node) {
    if !matches!(node.contents, Contents::Unread) {
        return;
    }
    node.contents = match &node.base {
        JsonValue::Array(values) => Contents::Array(ArrayOverlay::new(values.clone())),
        JsonValue::Object(values) => Contents::Object(ObjectOverlay {
            base: values.clone(), entries: Vec::new(), deleted: HashSet::new(),
        }),
        JsonValue::Null | JsonValue::Bool(_) | JsonValue::Number(_) | JsonValue::String(_) => {
            Contents::Primitive
        }
    };
}
pub(super) fn materialize(node: &NodeRef) -> JsonValue {
    let node = node.borrow();
    if !node.dirty && !node.subtree_dirty {
        return node.base.clone();
    }
    let result = match &node.contents {
        Contents::Unread | Contents::Primitive => return node.base.clone(),
        Contents::Array(values) => JsonValue::array(values.values()),
        Contents::Object(values) => JsonValue::object(values.values(&node.readded)),
    };
    let same_order = match (&result, &node.base) {
        (JsonValue::Object(a), JsonValue::Object(b)) => {
            a.iter().map(|(k, _)| k).eq(b.iter().map(|(k, _)| k))
        }
        _ => true,
    };
    if result == node.base && same_order && node.readded.is_empty() {
        node.base.clone()
    } else {
        result
    }
}

#[derive(Clone)]
pub struct Draft {
    overlay: Rc<Overlay>,
    node: NodeRef,
    ancestors: Vec<Weak<RefCell<Node>>>,
}
impl Draft {
    fn mark_dirty(&self) {
        let mut node = self.node.borrow_mut();
        if !node.dirty {
            node.dirty = true;
            node.dirty_order = self.overlay.dirty.borrow().len();
            self.overlay
                .dirty
                .borrow_mut()
                .push(Rc::downgrade(&self.node));
        }
        drop(node);
        for parent in self.ancestors.iter().filter_map(|p| p.upgrade()) {
            parent.borrow_mut().subtree_dirty = true;
        }
    }
    pub fn same_identity(&self, other: &Draft) -> bool {
        Rc::ptr_eq(&self.overlay, &other.overlay) && Rc::ptr_eq(&self.node, &other.node)
    }
    pub fn value(&self) -> Result<JsonValue> {
        self.overlay.readable()?;
        Ok(materialize(&self.node))
    }
    pub fn get(&self, segment: impl Into<Seg>) -> Result<Option<Self>> {
        self.overlay.readable()?;
        let seg = segment.into();
        let mut node = self.node.borrow_mut();
        load(&mut node);
        let child = match &mut node.contents {
            Contents::Object(entries) => {
                let key = super::key(&seg);
                entries.get(&key)
            }
            Contents::Array(entries) => match seg {
                Seg::Index(i) => entries.get(i).map(|e| e.node.clone()),
                Seg::Key(_) => return Err(DeltaError::UnsafePath(seg)),
            },
            Contents::Unread | Contents::Primitive => return Err(DeltaError::Path(vec![seg])),
        };
        Ok(child.map(|node| {
            self.overlay.nodes.borrow_mut().push(Rc::downgrade(&node));
            Self {
                overlay: self.overlay.clone(),
                node,
                ancestors: self
                    .ancestors
                    .iter()
                    .cloned()
                    .chain(std::iter::once(Rc::downgrade(&self.node)))
                    .collect(),
            }
        }))
    }
    pub fn at(&self, path: &[Seg]) -> Result<Self> {
        let mut current = self.clone();
        for seg in path {
            current = current
                .get(seg.clone())?
                .ok_or_else(|| DeltaError::Path(path.to_vec()))?;
        }
        Ok(current)
    }
    pub fn set(&self, segment: impl Into<Seg>, value: &JsonValue) -> Result<()> {
        self.overlay.writable()?;
        self.mark_dirty();
        let value = copy_json(value, CopyJsonOptions::default());
        let seg = segment.into();
        let mut node = self.node.borrow_mut();
        load(&mut node);
        match &mut node.contents {
            Contents::Object(entries) => {
                let key = super::key(&seg);
                let _previous = entries.get(&key);
                entries.deleted.remove(&key);
                if let Some(entry) = entries.entries.iter_mut().find(|e| e.key == key) {
                    if !value.is_container() && materialize(&entry.node) == value {
                        return Ok(());
                    }
                    entry.node = self::node(value);
                    entry.original = false;
                } else {
                    entries.entries.push(Entry {
                        key: key.clone(),
                        node: self::node(value),
                        original: false,
                    });
                }
                if node.deletes.contains(&key) {
                    node.deletes.retain(|k| k != &key);
                    if let JsonValue::Object(base) = &node.base {
                        if base.iter().any(|(k, _)| k == &key) && !node.readded.contains(&key) {
                            node.readded.push(key.clone());
                        }
                    }
                }
                if !node.writes.contains(&key) {
                    node.writes.push(key);
                }
            }
            Contents::Array(entries) => {
                let Seg::Index(i) = seg else {
                    return Err(DeltaError::UnsafePath(seg));
                };
                if i > entries.len() {
                    return Err(DeltaError::UnsafePath(Seg::Index(i)));
                }
                if i == entries.len() {
                    entries.set(i, self::node(value));
                } else {
                    entries.set(i, self::node(value));
                }
            }
            Contents::Unread | Contents::Primitive => return Err(DeltaError::Path(vec![seg])),
        }
        Ok(())
    }
    pub fn place(&self, segment: impl Into<Seg>, value: &Draft) -> Result<()> {
        self.set(segment, &value.value()?)
    }
    pub fn delete(&self, segment: impl Into<Seg>) -> Result<()> {
        self.overlay.writable()?;
        self.mark_dirty();
        let seg = segment.into();
        let key = super::key(&seg);
        let mut node = self.node.borrow_mut();
        load(&mut node);
        let Contents::Object(entries) = &mut node.contents else {
            return Err(DeltaError::Shape("draft arrays cannot contain holes"));
        };
        if entries.get(&key).is_some() {
            entries.entries.retain(|entry| entry.key != key);
            entries.deleted.insert(key.clone());
            node.writes.retain(|k| k != &key);
            node.readded.retain(|k| k != &key);
            if !node.deletes.contains(&key) {
                node.deletes.push(key);
            }
        }
        Ok(())
    }
    pub fn len(&self) -> Result<usize> {
        self.overlay.readable()?;
        let mut node = self.node.borrow_mut();
        load(&mut node);
        match &node.contents {
            Contents::Array(v) => Ok(v.len()),
            Contents::Object(v) => Ok(v.keys(&node.readded).len()),
            Contents::Unread | Contents::Primitive => Err(DeltaError::Shape("not a container")),
        }
    }
    pub fn is_empty(&self) -> Result<bool> {
        Ok(self.len()? == 0)
    }
    pub fn splice(&self, start: isize, remove: usize, items: &[JsonValue]) -> Result<Vec<Draft>> {
        self.overlay.writable()?;
        self.mark_dirty();
        let inserted: Vec<_> = items
            .iter()
            .map(|v| ArrayEntry {
                node: self::node(copy_json(v, CopyJsonOptions::default())),
                origin: None,
            })
            .collect();
        let mut node = self.node.borrow_mut();
        load(&mut node);
        let Contents::Array(entries) = &mut node.contents else {
            return Err(DeltaError::Shape("not an array"));
        };
        let start = clamp(start, entries.len());
        let end = start.saturating_add(remove).min(entries.len());
        Ok(entries
            .splice(start, end, inserted)
            .into_iter()
            .map(|e| {
                self.overlay.nodes.borrow_mut().push(Rc::downgrade(&e.node));
                Draft {
                    overlay: self.overlay.clone(),
                    node: e.node,
                    ancestors: Vec::new(),
                }
            })
            .collect())
    }
    pub fn push(&self, items: &[JsonValue]) -> Result<usize> {
        self.splice(isize::MAX, 0, items)?;
        self.len()
    }
    pub fn pop(&self) -> Result<Option<Draft>> {
        if self.is_empty()? {
            return Ok(None);
        }
        Ok(self.splice(-1, 1, &[])?.pop())
    }
    pub fn shift(&self) -> Result<Option<Draft>> {
        Ok(self.splice(0, 1, &[])?.pop())
    }
    pub fn unshift(&self, items: &[JsonValue]) -> Result<usize> {
        self.splice(0, 0, items)?;
        self.len()
    }
    pub fn set_length(&self, length: usize) -> Result<()> {
        let current = self.len()?;
        if length < current {
            self.splice(
                isize::try_from(length).map_err(|_| DeltaError::Shape("array length"))?,
                current - length,
                &[],
            )?;
        } else if length > current {
            self.push(&vec![JsonValue::Null; length - current])?;
        }
        Ok(())
    }
    pub fn reverse(&self) -> Result<()> {
        self.overlay.writable()?;
        self.mark_dirty();
        let mut node = self.node.borrow_mut();
        load(&mut node);
        let Contents::Array(entries) = &mut node.contents else {
            return Err(DeltaError::Shape("not an array"));
        };
        entries.reverse();
        Ok(())
    }
    pub fn sort_by(
        &self,
        mut compare: impl FnMut(&JsonValue, &JsonValue) -> std::cmp::Ordering,
    ) -> Result<()> {
        self.overlay.writable()?;
        self.mark_dirty();
        let mut entries = {
            let mut node = self.node.borrow_mut();
            load(&mut node);
            let Contents::Array(entries) = &node.contents else {
                return Err(DeltaError::Shape("not an array"));
            };
            entries.iter().collect::<Vec<_>>()
        };
        entries.sort_by(|a, b| compare(&materialize(&a.node), &materialize(&b.node)));
        self.overlay.writable()?;
        let mut node = self.node.borrow_mut();
        let Contents::Array(current) = &mut node.contents else {
            return Err(DeltaError::Shape("not an array"));
        };
        let copied = entries.len().min(current.len());
        let mut tail: Vec<_> = current.iter().skip(copied).collect();
        entries.append(&mut tail);
        let mut seen = HashSet::new();
        for entry in entries.iter_mut() {
            if !seen.insert(Rc::as_ptr(&entry.node)) {
                entry.node = self::node(copy_json(
                    &materialize(&entry.node),
                    CopyJsonOptions::default(),
                ));
                entry.origin = None;
            }
        }
        current.reorder(entries);
        Ok(())
    }
    pub fn sort(&self) -> Result<()> {
        self.sort_by(|a, b| js_string(a).cmp(&js_string(b)))
    }
    pub fn splice_to_end(&self, start: isize) -> Result<Vec<Draft>> {
        self.splice(start, usize::MAX, &[])
    }
    pub fn keys(&self) -> Result<Vec<crate::types::JsString>> {
        self.overlay.readable()?;
        let mut node = self.node.borrow_mut();
        load(&mut node);
        match &node.contents {
            Contents::Object(entries) => {
                Ok(entries.keys(&node.readded))
            }
            Contents::Array(entries) => Ok((0..entries.len()).map(|i| i.to_string().into()).collect()),
            Contents::Unread | Contents::Primitive => Err(DeltaError::Shape("not a container")),
        }
    }
    pub fn fill(&self, value: &JsonValue, start: isize, end: Option<isize>) -> Result<()> {
        let length = self.len()?;
        let start = clamp(start, length);
        let end = end.map_or(length, |e| clamp(e, length));
        if end > start {
            self.splice(
                isize::try_from(start).map_err(|_| DeltaError::Shape("index"))?,
                end - start,
                &vec![value.clone(); end - start],
            )?;
        }
        Ok(())
    }
    pub fn copy_within(&self, target: isize, start: isize, end: Option<isize>) -> Result<()> {
        let length = self.len()?;
        let target = clamp(target, length);
        let start = clamp(start, length);
        let end = end.map_or(length, |e| clamp(e, length));
        let count = end.saturating_sub(start).min(length - target);
        let mut values = Vec::with_capacity(count);
        for i in start..start + count {
            values.push(self.get(i)?.ok_or(DeltaError::Shape("index"))?.value()?);
        }
        self.splice(
            isize::try_from(target).map_err(|_| DeltaError::Shape("index"))?,
            count,
            &values,
        )?;
        Ok(())
    }
}
fn clamp(index: isize, length: usize) -> usize {
    if index < 0 {
        length.saturating_sub(index.unsigned_abs())
    } else {
        usize::try_from(index).unwrap_or(usize::MAX).min(length)
    }
}
fn js_string(value: &JsonValue) -> crate::types::JsString {
    match value {
        JsonValue::Null => "null".into(),
        JsonValue::Bool(v) => v.to_string().into(),
        JsonValue::Number(v) => v.to_string().into(),
        JsonValue::String(v) => v.clone(),
        JsonValue::Object(_) => "[object Object]".into(),
        JsonValue::Array(values) => {
            let mut result = crate::types::JsString::from("");
            for (index, value) in values.iter().enumerate() {
                if index != 0 {
                    result = result.concat(&",".into());
                }
                if !matches!(value, JsonValue::Null) {
                    result = result.concat(&js_string(value));
                }
            }
            result
        }
    }
}

pub struct Change {
    overlay: Rc<Overlay>,
    root: Option<NodeRef>,
}
impl Change {
    pub fn state(&self) -> Result<Draft> {
        self.overlay.writable()?;
        Ok(Draft {
            overlay: self.overlay.clone(),
            node: self.root.clone().ok_or(DeltaError::Settled)?,
            ancestors: Vec::new(),
        })
    }
    pub fn prepare(&mut self) -> Result<Prepared> {
        self.overlay.writable()?;
        let root = self.root.take().ok_or(DeltaError::Settled)?;
        let base = root.borrow().base.clone();
        let mut operations = Vec::new();
        let mut pending = Schedule::default();
        pending.push(root.clone(), Vec::new());
        while let Some((node, path)) = pending.pop() {
            emit_node(&node, &path, &mut operations, &mut pending);
            if operations.len() > 4_096 {
                break;
            }
        }
        if operations.len() > 4_096 {
            operations = vec![Op::Replace(materialize(&root))];
        }
        let value = match apply_immutable_trusted(&base, &operations) {
            Ok(value) => value,
            Err(error) => {
                self.overlay.status.set(Status::Aborted);
                self.overlay.release();
                return Err(error);
            }
        };
        self.overlay.status.set(Status::Prepared);
        self.overlay.release();
        Ok(Prepared {
            overlay: self.overlay.clone(),
            base,
            value,
            ops: operations,
        })
    }
    pub fn abort(&mut self) {
        match self.overlay.status.get() {
            Status::Open | Status::Prepared => self.overlay.status.set(Status::Aborted),
            Status::Consumed | Status::Aborted | Status::Stale => {}
        }
        self.root = None;
        self.overlay.release();
    }
}
pub struct Prepared {
    overlay: Rc<Overlay>,
    base: JsonValue,
    value: JsonValue,
    ops: Vec<Op>,
}
impl Prepared {
    pub fn base(&self) -> &JsonValue {
        &self.base
    }
    pub fn value(&self) -> &JsonValue {
        &self.value
    }
    pub fn ops(&self) -> &[Op] {
        &self.ops
    }
    pub fn base_revision(&self) -> u64 {
        self.overlay.revision
    }
    pub fn abort(&self) {
        if self.overlay.status.get() == Status::Prepared {
            self.overlay.status.set(Status::Aborted);
            self.overlay.release();
        }
    }
}
pub struct Tracker {
    authority: Rc<Authority>,
    value: JsonValue,
}
pub fn track(initial: JsonValue) -> Result<Tracker> {
    if !initial.is_container() {
        return Err(DeltaError::Root);
    }
    Ok(Tracker {
        authority: Rc::new(Authority {
            revision: Cell::new(0),
            contexts: RefCell::new(Vec::new()),
        }),
        value: initial,
    })
}
impl Tracker {
    pub fn value(&self) -> &JsonValue {
        &self.value
    }
    pub fn revision(&self) -> u64 {
        self.authority.revision.get()
    }
    fn overlay(&self) -> Rc<Overlay> {
        let overlay = Rc::new(Overlay {
            authority: Rc::downgrade(&self.authority),
            revision: self.revision(),
            status: Cell::new(Status::Open),
            dirty: RefCell::new(Vec::new()),
            nodes: RefCell::new(Vec::new()),
        });
        let mut contexts = self.authority.contexts.borrow_mut();
        contexts.retain(|v| v.strong_count() > 0);
        contexts.push(Rc::downgrade(&overlay));
        overlay
    }
    pub fn begin_change(&self) -> Change {
        let overlay = self.overlay();
        let root = node(self.value.clone());
        overlay.nodes.borrow_mut().push(Rc::downgrade(&root));
        Change {
            overlay,
            root: Some(root),
        }
    }
    pub fn prepare_replace(&self, value: JsonValue) -> Result<Prepared> {
        if !value.is_container() {
            return Err(DeltaError::Root);
        }
        let overlay = self.overlay();
        overlay.status.set(Status::Prepared);
        let ops = if self.value == value {
            Vec::new()
        } else {
            vec![Op::Replace(value.clone())]
        };
        let value = if ops.is_empty() {
            self.value.clone()
        } else {
            value
        };
        Ok(Prepared {
            overlay,
            base: self.value.clone(),
            value,
            ops,
        })
    }
    pub fn adopt(&mut self, prepared: &Prepared) -> Result<()> {
        if !prepared
            .overlay
            .authority
            .ptr_eq(&Rc::downgrade(&self.authority))
        {
            return Err(DeltaError::Foreign);
        }
        match prepared.overlay.status.get() {
            Status::Consumed => return Err(DeltaError::Consumed),
            Status::Aborted => return Err(DeltaError::Aborted),
            Status::Stale => return Err(DeltaError::Stale),
            Status::Open => return Err(DeltaError::Settled),
            Status::Prepared => {}
        }
        if prepared.base_revision() != self.revision() {
            prepared.overlay.status.set(Status::Stale);
            return Err(DeltaError::Stale);
        }
        self.value = prepared.value.clone();
        prepared.overlay.status.set(Status::Consumed);
        self.authority.revision.set(self.revision() + 1);
        for overlay in self
            .authority
            .contexts
            .borrow_mut()
            .drain(..)
            .filter_map(|v| v.upgrade())
        {
            if !Rc::ptr_eq(&overlay, &prepared.overlay)
                && matches!(overlay.status.get(), Status::Open | Status::Prepared)
            {
                overlay.status.set(Status::Stale);
                overlay.release();
            }
        }
        Ok(())
    }
}
fn child_path(path: &[Seg], segment: Seg) -> Path {
    let mut p = path.to_vec();
    p.push(segment);
    p
}
fn changed_value(
    before: Option<&JsonValue>,
    after: JsonValue,
    path: Path,
    operations: &mut Vec<Op>,
) {
    if before == Some(&after) {
        return;
    }
    if let (Some(JsonValue::String(a)), JsonValue::String(b)) = (before, &after) {
        operations.extend(emit_string(a, b, &path));
    } else {
        operations.push(Op::Set(path, after));
    }
}
#[derive(Default)]
struct Schedule {
    nodes: std::collections::BTreeMap<(usize, usize, usize), (NodeRef, Path)>,
    serial: usize,
}
impl Schedule {
    fn push(&mut self, node: NodeRef, path: Path) {
        let order = node.borrow().dirty_order;
        self.nodes.insert((path.len(), order, self.serial), (node, path));
        self.serial += 1;
    }
    fn pop(&mut self) -> Option<(NodeRef, Path)> {
        self.nodes.pop_first().map(|(_, value)| value)
    }
}
fn emit_node(
    reference: &NodeRef,
    path: &[Seg],
    operations: &mut Vec<Op>,
    pending: &mut Schedule,
) {
    if operations.len() > 4_096 {
        return;
    }
    let node = reference.borrow();
    if !node.dirty && !node.subtree_dirty {
        return;
    }
    match &node.contents {
        Contents::Unread | Contents::Primitive => {}
        Contents::Object(entries) => {
            let reserved = node
                .writes
                .iter()
                .chain(node.deletes.iter())
                .any(|k| k.reserved());
            let reserved_child = entries.entries.iter().filter(|e| e.original).any(|entry| {
                entry.key.reserved()
                    && materialize(&entry.node) != entry.node.borrow().base
            });
            if reserved || reserved_child {
                operations.push(if path.is_empty() {
                    Op::Replace(materialize(reference))
                } else {
                    Op::Set(path.to_vec(), materialize(reference))
                });
                return;
            }
            let JsonValue::Object(base) = &node.base else {
                return;
            };
            for key in &node.readded {
                operations.push(Op::Delete(child_path(path, Seg::Key(key.clone()))));
            }
            for key in &node.writes {
                if let Some(entry) = entries.entries.iter().find(|e| &e.key == key) {
                    let before = if node.readded.contains(key) {
                        None
                    } else {
                        base.iter().find(|(k, _)| k == key).map(|(_, v)| v)
                    };
                    changed_value(
                        before,
                        materialize(&entry.node),
                        child_path(path, Seg::Key(key.clone())),
                        operations,
                    );
                }
            }
            for key in &node.deletes {
                if base.iter().any(|(k, _)| k == key) {
                    operations.push(Op::Delete(child_path(path, Seg::Key(key.clone()))));
                }
            }
            for entry in entries.entries.iter().filter(|e| e.original) {
                if !entry.node.borrow().dirty && !entry.node.borrow().subtree_dirty {
                    continue;
                }
                let child = child_path(path, Seg::Key(entry.key.clone()));
                pending.push(entry.node.clone(), child);
            }
        }
        Contents::Array(entries) => {
            let JsonValue::Array(base) = &node.base else {
                return;
            };
            let structural = entries.structural;
            if structural {
                let origins = entries.origins();
                let retained: HashSet<usize> = origins.iter().filter_map(|e| *e).collect();
                let mut end = base.len();
                while end > 0 {
                    if retained.contains(&(end - 1)) {
                        end -= 1;
                        continue;
                    }
                    let mut start = end - 1;
                    while start > 0 && !retained.contains(&(start - 1)) {
                        start -= 1;
                    }
                    operations.push(Op::Splice(path.to_vec(), start, end - start, Vec::new()));
                    end = start;
                }
                let retained_base: Vec<_> =
                    (0..base.len()).filter(|i| retained.contains(i)).collect();
                let target: Vec<_> = origins.iter().filter_map(|e| *e).collect();
                if retained_base != target {
                    let positions: std::collections::HashMap<_, _> = retained_base
                        .iter()
                        .enumerate()
                        .map(|(at, index)| (*index, at))
                        .collect();
                    let order = target
                        .iter()
                        .filter_map(|i| positions.get(i).copied())
                        .collect();
                    operations.push(Op::Permute(path.to_vec(), order));
                }
                let mut index = 0;
                while index < entries.len() {
                    if origins[index].is_some() {
                        index += 1;
                        continue;
                    }
                    let start = index;
                    let mut values = Vec::new();
                    while index < origins.len() && origins[index].is_none() {
                        let Some(entry) = entries.get(index) else {
                            break;
                        };
                        values.push(materialize(&entry.node));
                        index += 1;
                    }
                    operations.push(Op::Splice(path.to_vec(), start, 0, values));
                }
            }
            let dirty: Vec<_> = entries
                .accessed()
                .into_iter()
                .filter_map(|(i, e)| {
                    e.origin
                        .filter(|origin| materialize(&e.node) != base[*origin])
                        .map(|_| i)
                })
                .collect();
            let dense = !structural && dirty.len() >= 256;
            let mut covered = HashSet::new();
            if dense {
                let mut at = 0;
                while at < dirty.len() {
                    let start = dirty[at];
                    let mut end = at + 1;
                    while end < dirty.len() && dirty[end] - dirty[end - 1] <= 2 {
                        end += 1;
                    }
                    if end - at >= 256 {
                        let last = dirty[end - 1] + 1;
                        operations.push(Op::Splice(
                            path.to_vec(),
                            start,
                            last - start,
                            entries
                                .iter()
                                .skip(start)
                                .take(last - start)
                                .map(|e| materialize(&e.node))
                                .collect(),
                        ));
                        covered.extend(start..last);
                    }
                    at = end;
                }
            }
            for (index, entry) in entries.accessed() {
                let Some(origin) = entry.origin else {
                    continue;
                };
                if covered.contains(&index) {
                    continue;
                }
                let child = child_path(path, Seg::Index(index));
                if entry.node.borrow().base.same_identity(&base[origin]) {
                    if entry.node.borrow().dirty || entry.node.borrow().subtree_dirty {
                        pending.push(entry.node.clone(), child);
                    }
                } else {
                    changed_value(
                        Some(&base[origin]),
                        materialize(&entry.node),
                        child,
                        operations,
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod piece_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn wide_object_reads_allocate_only_accessed_overlay_entries() -> Result<()> {
        let tracker = track(JsonValue::object((0..40_000).map(|i| {
            (format!("field{i}").into(), json!({"value":i}).into())
        }).collect()))?;
        let mut change = tracker.begin_change();
        let draft = change.state()?;
        draft.get("field20000")?.ok_or(DeltaError::Root)?.set("value", &json!(-1).into())?;
        {
            let node = draft.node.borrow();
            let Contents::Object(object) = &node.contents else { return Err(DeltaError::Root); };
            assert_eq!(object.entries.len(), 1);
        }
        let prepared = change.prepare()?;
        assert_eq!(prepared.ops(), &[Op::Set(vec!["field20000".into(), "value".into()], json!(-1).into())]);
        Ok(())
    }

    #[test]
    fn length_and_sparse_get_do_not_materialize_the_base_array() -> Result<()> {
        let tracker = track(JsonValue::array(
            (0..40_000)
                .map(|value| json!({"value":value}).into())
                .collect(),
        ))?;
        let mut change = tracker.begin_change();
        let draft = change.state()?;
        assert_eq!(draft.len()?, 40_000);
        {
            let node = draft.node.borrow();
            let Contents::Array(array) = &node.contents else {
                return Err(DeltaError::Root);
            };
            assert!(array.accessed().is_empty());
        }
        draft
            .get(20_000usize)?
            .ok_or(DeltaError::Root)?
            .set("value", &json!(-1).into())?;
        {
            let node = draft.node.borrow();
            let Contents::Array(array) = &node.contents else {
                return Err(DeltaError::Root);
            };
            assert_eq!(array.accessed().len(), 1);
        }
        let prepared = change.prepare()?;
        assert_eq!(
            prepared.ops(),
            &[Op::Set(
                vec![20_000usize.into(), "value".into()],
                json!(-1).into()
            )]
        );
        Ok(())
    }
}
