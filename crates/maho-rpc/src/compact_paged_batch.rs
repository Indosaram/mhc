//! senpi `modes/rpc/compact-paged-batch.ts`: one compaction round's sorted changes (`None` deletes)
//! merged into the AVL in one descent, with join-based rebalancing (senpi#4272, RULING-47). Changed
//! nodes stay drafts until the final tree is made bottom-up, so each node of the result is built,
//! owned and registered for commit exactly once; a node replaced inside the round is never built.
use crate::compact_paged_tree::{CompactPagedTree, PagedNode, PagedRoot};
use crate::compact_read_lease::CompactError;
use crate::compact_values::ValueRef;
use futures::future::BoxFuture;
use serde::Serialize;
use serde::de::DeserializeOwned;

/// A changed node of the round being built: plain RAM, mutated in place until it is made once.
struct Draft<T> {
    /// `None` for a key new to the tree; its reference is owned only while the node is made.
    key: Option<ValueRef>,
    name: String,
    value: T,
    left: Sub<T>,
    right: Sub<T>,
    height: i64,
    size: i64,
}

enum Sub<T> {
    Empty,
    Root(PagedRoot),
    Draft(Box<Draft<T>>),
}

type MinSplit<T> = (Box<Draft<T>>, Sub<T>);
type TakeMinResult<T> = Result<MinSplit<T>, CompactError>;

impl<T> Sub<T> {
    fn height(&self) -> i64 {
        match self {
            Self::Empty => 0,
            Self::Root(root) => root.height,
            Self::Draft(draft) => draft.height,
        }
    }

    fn size(&self) -> i64 {
        match self {
            Self::Empty => 0,
            Self::Root(root) => root.size,
            Self::Draft(draft) => draft.size,
        }
    }

    fn from_root(root: Option<PagedRoot>) -> Self {
        root.map_or(Self::Empty, Self::Root)
    }
}

fn side<T>(draft: &mut Draft<T>, left: bool) -> &mut Sub<T> {
    if left {
        &mut draft.left
    } else {
        &mut draft.right
    }
}

/// Whether a merged subtree is still exactly the stored one (no change reached it).
fn same<T>(sub: &Sub<T>, stored: Option<&PagedRoot>) -> bool {
    match (sub, stored) {
        (Sub::Empty, None) => true,
        (Sub::Root(root), Some(stored)) => root.reference.node == stored.reference.node,
        _ => false,
    }
}

fn fix<T>(mut node: Box<Draft<T>>) -> Box<Draft<T>> {
    node.height = 1 + node.left.height().max(node.right.height());
    node.size = 1 + node.left.size() + node.right.size();
    node
}

pub struct CompactPagedBatch<'a, T> {
    tree: &'a CompactPagedTree<T>,
    changes: Vec<(String, Option<T>)>,
}

impl<'a, T: Clone + Serialize + DeserializeOwned + Send + Sync + 'static> CompactPagedBatch<'a, T> {
    pub fn new(tree: &'a CompactPagedTree<T>, mut changes: Vec<(String, Option<T>)>) -> Self {
        changes.sort_by(|(a, _), (b, _)| a.cmp(b));
        Self { tree, changes }
    }

    /// Returns an owned root; the caller keeps `root` live until this settles.
    pub async fn apply(self, root: Option<PagedRoot>) -> Result<Option<PagedRoot>, CompactError> {
        let count = self.changes.len();
        let merged = self.merge(root, 0, count).await?;
        self.build(merged).await
    }

    fn merge(
        &self,
        sub: Option<PagedRoot>,
        from: usize,
        to: usize,
    ) -> BoxFuture<'_, Result<Sub<T>, CompactError>> {
        Box::pin(async move {
            if from == to {
                return Ok(Sub::from_root(sub));
            }
            let Some(root) = sub else {
                return Ok(self.fresh(from, to));
            };
            let node = self.tree.read(&root).await?;
            let name = node.name.clone();
            let split = from + self.changes[from..to].partition_point(|(key, _)| *key < name);
            let hit = split < to && self.changes[split].0 == name;
            let right_from = if hit { split + 1 } else { split };
            let left = self.merge(node.node.left.clone(), from, split).await?;
            let right = self.merge(node.node.right.clone(), right_from, to).await?;
            if !hit
                && same(&left, node.node.left.as_ref())
                && same(&right, node.node.right.as_ref())
            {
                return Ok(Sub::Root(root));
            }
            let value = if hit {
                self.changes[split].1.clone()
            } else {
                Some(node.node.value.clone())
            };
            let Some(value) = value else {
                return self.concat(left, right).await;
            };
            let draft = Box::new(Draft {
                key: Some(node.node.key.clone()),
                name,
                value,
                left: Sub::Empty,
                right: Sub::Empty,
                height: root.height,
                size: root.size,
            });
            Ok(Sub::Draft(self.join(left, draft, right).await?))
        })
    }

    /// A perfectly balanced subtree of the inserts in an empty position; deletes of absent keys are no-ops.
    fn fresh(&self, from: usize, to: usize) -> Sub<T> {
        let rows: Vec<(String, T)> = self.changes[from..to]
            .iter()
            .filter_map(|(name, value)| value.clone().map(|value| (name.clone(), value)))
            .collect();
        fn grow<T>(rows: &[(String, T)]) -> Sub<T>
        where
            T: Clone,
        {
            if rows.is_empty() {
                return Sub::Empty;
            }
            let mid = rows.len() / 2;
            let (name, value) = rows[mid].clone();
            Sub::Draft(fix(Box::new(Draft {
                key: None,
                name,
                value,
                left: grow(&rows[..mid]),
                right: grow(&rows[mid + 1..]),
                height: 0,
                size: 0,
            })))
        }
        grow(&rows)
    }

    /// AVL join: descends the taller side's spine to a height match, rebalancing once per level.
    fn join(
        &self,
        left: Sub<T>,
        mut node: Box<Draft<T>>,
        right: Sub<T>,
    ) -> BoxFuture<'_, Result<Box<Draft<T>>, CompactError>> {
        Box::pin(async move {
            let (lh, rh) = (left.height(), right.height());
            if lh > rh + 1 {
                let mut top = self.open(left).await?;
                let inner = std::mem::replace(&mut top.right, Sub::Empty);
                top.right = Sub::Draft(self.join(inner, node, right).await?);
                return self.balance(top).await;
            }
            if rh > lh + 1 {
                let mut top = self.open(right).await?;
                let inner = std::mem::replace(&mut top.left, Sub::Empty);
                top.left = Sub::Draft(self.join(left, node, inner).await?);
                return self.balance(top).await;
            }
            node.left = left;
            node.right = right;
            Ok(fix(node))
        })
    }

    async fn concat(&self, left: Sub<T>, right: Sub<T>) -> Result<Sub<T>, CompactError> {
        if matches!(left, Sub::Empty) {
            return Ok(right);
        }
        if matches!(right, Sub::Empty) {
            return Ok(left);
        }
        let (min, rest) = self.take_min(right).await?;
        Ok(Sub::Draft(self.join(left, min, rest).await?))
    }

    fn take_min(&self, sub: Sub<T>) -> BoxFuture<'_, TakeMinResult<T>> {
        Box::pin(async move {
            let mut node = self.open(sub).await?;
            if matches!(node.left, Sub::Empty) {
                let rest = std::mem::replace(&mut node.right, Sub::Empty);
                return Ok((node, rest));
            }
            let left = std::mem::replace(&mut node.left, Sub::Empty);
            let (min, rest) = self.take_min(left).await?;
            node.left = rest;
            Ok((min, Sub::Draft(self.balance(node).await?)))
        })
    }

    /// One single or double rotation restores a node whose children differ in height by two.
    async fn balance(&self, mut node: Box<Draft<T>>) -> Result<Box<Draft<T>>, CompactError> {
        let left_heavy = node.left.height() > node.right.height();
        let (s, o) = (left_heavy, !left_heavy);
        let heavy_height = side(&mut node, s).height();
        if heavy_height == 0 || heavy_height - side(&mut node, o).height() < 2 {
            return Ok(fix(node));
        }
        let heavy = std::mem::replace(side(&mut node, s), Sub::Empty);
        let mut child = self.open(heavy).await?;
        let inner = std::mem::replace(side(&mut child, o), Sub::Empty);
        if inner.height() > side(&mut child, s).height() {
            let mut pivot = self.open(inner).await?;
            *side(&mut child, o) = std::mem::replace(side(&mut pivot, s), Sub::Empty);
            *side(&mut node, s) = std::mem::replace(side(&mut pivot, o), Sub::Empty);
            *side(&mut pivot, s) = Sub::Draft(fix(child));
            *side(&mut pivot, o) = Sub::Draft(fix(node));
            return Ok(fix(pivot));
        }
        *side(&mut node, s) = inner;
        *side(&mut child, o) = Sub::Draft(fix(node));
        Ok(fix(child))
    }

    async fn open(&self, sub: Sub<T>) -> Result<Box<Draft<T>>, CompactError> {
        match sub {
            Sub::Draft(draft) => Ok(draft),
            Sub::Root(root) => {
                let decoded = self.tree.read(&root).await?;
                let PagedNode {
                    key,
                    value,
                    left,
                    right,
                } = decoded.node;
                Ok(Box::new(Draft {
                    key: Some(key),
                    name: decoded.name,
                    value,
                    left: Sub::from_root(left),
                    right: Sub::from_root(right),
                    height: root.height,
                    size: root.size,
                }))
            }
            Sub::Empty => Err(crate::compact_read_lease::CompactError::read(
                crate::compact_read_lease::CompactReadReason::Ownership,
            )),
        }
    }

    /// Makes each draft once, children first; every result and intermediate is owned exactly once.
    fn build(&self, sub: Sub<T>) -> BoxFuture<'_, Result<Option<PagedRoot>, CompactError>> {
        Box::pin(async move {
            let draft = match sub {
                Sub::Empty => return Ok(None),
                Sub::Root(root) => {
                    self.tree.adjust(Some(&root), 1, 0)?;
                    return Ok(Some(root));
                }
                Sub::Draft(draft) => draft,
            };
            let Draft {
                key,
                name,
                value,
                left,
                right,
                ..
            } = *draft;
            let left = self.build(left).await?;
            let result = async {
                let right = self.build(right).await?;
                let made = async {
                    let fresh = key.is_none();
                    let key = match key {
                        Some(key) => key,
                        None => self.tree.own_name(&name)?,
                    };
                    let made = self
                        .tree
                        .make(PagedNode {
                            key: key.clone(),
                            value,
                            left: left.clone(),
                            right: right.clone(),
                        })
                        .await;
                    if fresh {
                        self.tree.store.adjust(key.owner, -1, 0)?;
                    }
                    made
                }
                .await;
                self.tree.adjust(right.as_ref(), -1, 0)?;
                made
            }
            .await;
            self.tree.adjust(left.as_ref(), -1, 0)?;
            result.map(Some)
        })
    }
}
