//! senpi `modes/rpc/compact-index-tree.ts`: resident membership/path headers only; immutable values
//! have their own spillable ownership.
//!
//! Persistent AVL nodes are shared (`Arc`) between roots; each keeps upstream's mutable live/pin counts
//! as atomics, and only a count's category transition (live, pinned-only, dead) propagates to children
//! and the retained value.
use crate::compact_read_lease::{CompactError, CompactReadReason};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

pub struct IndexNode<T> {
    pub key: String,
    pub value: T,
    pub left: Option<Arc<IndexNode<T>>>,
    pub right: Option<Arc<IndexNode<T>>>,
    pub height: i64,
    pub size: i64,
    live: AtomicI64,
    pins: AtomicI64,
}

impl<T> IndexNode<T> {
    pub fn live(&self) -> i64 {
        self.live.load(Ordering::SeqCst)
    }

    pub fn pins(&self) -> i64 {
        self.pins.load(Ordering::SeqCst)
    }
}

pub type IndexRoot<T> = Option<Arc<IndexNode<T>>>;

fn height<T>(node: &IndexRoot<T>) -> i64 {
    node.as_ref().map_or(0, |node| node.height)
}

fn size<T>(node: &IndexRoot<T>) -> i64 {
    node.as_ref().map_or(0, |node| node.size)
}

type Retain<T> = Box<dyn Fn(&T, i64, i64) -> Result<(), CompactError> + Send + Sync>;

pub struct CompactIndexTree<T> {
    resident_nodes: AtomicI64,
    allocated_nodes: AtomicI64,
    retain: Retain<T>,
}

impl<T: Clone> CompactIndexTree<T> {
    pub fn new(
        retain: impl Fn(&T, i64, i64) -> Result<(), CompactError> + Send + Sync + 'static,
    ) -> Self {
        Self {
            resident_nodes: AtomicI64::new(0),
            allocated_nodes: AtomicI64::new(0),
            retain: Box::new(retain),
        }
    }

    pub fn resident_nodes(&self) -> i64 {
        self.resident_nodes.load(Ordering::SeqCst)
    }

    pub fn allocated_nodes(&self) -> i64 {
        self.allocated_nodes.load(Ordering::SeqCst)
    }

    pub fn get<'a>(&self, root: &'a IndexRoot<T>, key: &str) -> Option<&'a T> {
        let mut node = root.as_ref();
        while let Some(current) = node {
            if key == current.key {
                return Some(&current.value);
            }
            node = if key < current.key.as_str() {
                current.left.as_ref()
            } else {
                current.right.as_ref()
            };
        }
        None
    }

    pub fn adjust(&self, root: &IndexRoot<T>, live: i64, pins: i64) -> Result<(), CompactError> {
        let Some(root) = root else { return Ok(()) };
        let (old_live, old_pins) = (root.live(), root.pins());
        if old_live + live < 0 || old_pins + pins < 0 {
            return Err(CompactError::read(CompactReadReason::Ownership));
        }
        let (new_live, new_pins) = (old_live + live, old_pins + pins);
        root.live.store(new_live, Ordering::SeqCst);
        root.pins.store(new_pins, Ordering::SeqCst);
        let l = i64::from(new_live > 0) - i64::from(old_live > 0);
        let p = i64::from(new_live == 0 && new_pins > 0) - i64::from(old_live == 0 && old_pins > 0);
        if l != 0 || p != 0 {
            (self.retain)(&root.value, l, p)?;
            self.adjust(&root.left, l, p)?;
            self.adjust(&root.right, l, p)?;
        }
        if new_live == 0 && new_pins == 0 {
            self.resident_nodes.fetch_sub(1, Ordering::SeqCst);
        }
        Ok(())
    }

    pub fn set(
        &self,
        root: &IndexRoot<T>,
        key: &str,
        value: Option<T>,
    ) -> Result<IndexRoot<T>, CompactError> {
        let Some(node) = root else {
            return match value {
                None => Ok(None),
                Some(value) => self.make(key, value, &None, &None),
            };
        };
        if key == node.key {
            if let Some(value) = value {
                return self.make(key, value, &node.left, &node.right);
            }
            let (Some(_), Some(right)) = (&node.left, &node.right) else {
                let child = node.left.clone().or_else(|| node.right.clone());
                self.adjust(&child, 1, 0)?;
                return Ok(child);
            };
            let mut next = right.clone();
            while let Some(left) = next.left.clone() {
                next = left;
            }
            let right = self.set(&node.right, &next.key, None)?;
            let result = self.balance(&next.key, next.value.clone(), &node.left, &right)?;
            self.adjust(&right, -1, 0)?;
            return Ok(result);
        }
        let leftward = key < node.key.as_str();
        let child = self.set(if leftward { &node.left } else { &node.right }, key, value)?;
        let result = if leftward {
            self.balance(&node.key, node.value.clone(), &child, &node.right)?
        } else {
            self.balance(&node.key, node.value.clone(), &node.left, &child)?
        };
        self.adjust(&child, -1, 0)?;
        Ok(result)
    }

    /// In-order entries strictly after `after` (all when `None`).
    pub fn entries(&self, root: &IndexRoot<T>, after: Option<&str>) -> Vec<(String, T)> {
        let mut out = Vec::new();
        Self::collect(root, after, &mut out);
        out
    }

    fn collect(root: &IndexRoot<T>, after: Option<&str>, out: &mut Vec<(String, T)>) {
        let Some(node) = root else { return };
        if after.is_none_or(|after| node.key.as_str() > after) {
            Self::collect(&node.left, after, out);
            out.push((node.key.clone(), node.value.clone()));
        }
        Self::collect(&node.right, after, out);
    }

    fn make(
        &self,
        key: &str,
        value: T,
        left: &IndexRoot<T>,
        right: &IndexRoot<T>,
    ) -> Result<IndexRoot<T>, CompactError> {
        self.adjust(left, 1, 0)?;
        self.adjust(right, 1, 0)?;
        (self.retain)(&value, 1, 0)?;
        self.resident_nodes.fetch_add(1, Ordering::SeqCst);
        self.allocated_nodes.fetch_add(1, Ordering::SeqCst);
        Ok(Some(Arc::new(IndexNode {
            key: key.to_owned(),
            value,
            left: left.clone(),
            right: right.clone(),
            height: 1 + height(left).max(height(right)),
            size: 1 + size(left) + size(right),
            live: AtomicI64::new(1),
            pins: AtomicI64::new(0),
        })))
    }

    fn balance(
        &self,
        key: &str,
        value: T,
        left: &IndexRoot<T>,
        right: &IndexRoot<T>,
    ) -> Result<IndexRoot<T>, CompactError> {
        if let Some(l) = left.as_ref().filter(|l| l.height > height(right) + 1) {
            if let Some(middle) = l.right.as_ref().filter(|m| m.height > height(&l.left)) {
                let a = self.make(&l.key, l.value.clone(), &l.left, &middle.left)?;
                let b = self.make(key, value, &middle.right, right)?;
                let result = self.make(&middle.key, middle.value.clone(), &a, &b)?;
                self.adjust(&a, -1, 0)?;
                self.adjust(&b, -1, 0)?;
                return Ok(result);
            }
            let b = self.make(key, value, &l.right, right)?;
            let result = self.make(&l.key, l.value.clone(), &l.left, &b)?;
            self.adjust(&b, -1, 0)?;
            return Ok(result);
        }
        if let Some(r) = right.as_ref().filter(|r| r.height > height(left) + 1) {
            if let Some(middle) = r.left.as_ref().filter(|m| m.height > height(&r.right)) {
                let a = self.make(key, value, left, &middle.left)?;
                let b = self.make(&r.key, r.value.clone(), &middle.right, &r.right)?;
                let result = self.make(&middle.key, middle.value.clone(), &a, &b)?;
                self.adjust(&a, -1, 0)?;
                self.adjust(&b, -1, 0)?;
                return Ok(result);
            }
            let a = self.make(key, value, left, &r.left)?;
            let result = self.make(&r.key, r.value.clone(), &a, &r.right)?;
            self.adjust(&a, -1, 0)?;
            return Ok(result);
        }
        self.make(key, value, left, right)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, HashMap, HashSet};
    use std::sync::Mutex;

    type Value = (i64, u32);

    // senpi 4272-index-tree-property / RULING-35: deterministic state machine across sharing and refcount-to-zero.
    #[test]
    fn preserves_avl_and_map_invariants_through_2048_operations() {
        for mut seed in [1u32, 0x1234_5678, 0xdead_beef] {
            let retained: Arc<Mutex<HashMap<Value, (i64, i64)>>> = Arc::default();
            let counts = retained.clone();
            let tree = CompactIndexTree::<Value>::new(move |value, live, pins| {
                let mut counts = counts.lock().expect("counts");
                let entry = counts.entry(*value).or_default();
                entry.0 += live;
                entry.1 += pins;
                assert!(entry.0 >= 0 && entry.1 >= 0);
                if entry.0 == 0 && entry.1 == 0 {
                    counts.remove(value);
                }
                Ok(())
            });
            let mut random = move || {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                seed
            };
            let mut root: IndexRoot<Value> = None;
            let mut reference: BTreeMap<String, Value> = BTreeMap::new();
            let mut cuts: Vec<(IndexRoot<Value>, BTreeMap<String, Value>)> = Vec::new();
            fn inspect(
                node: &IndexRoot<Value>,
                seen: &mut HashSet<*const IndexNode<Value>>,
                keys: &mut Vec<String>,
            ) -> i64 {
                let Some(node) = node else { return 0 };
                seen.insert(Arc::as_ptr(node));
                let left = inspect(&node.left, seen, keys);
                keys.push(node.key.clone());
                let right = inspect(&node.right, seen, keys);
                assert!((left - right).abs() <= 1);
                assert_eq!(node.height, 1 + left.max(right));
                assert_eq!(node.size, 1 + size(&node.left) + size(&node.right));
                assert!(node.live() + node.pins() > 0);
                node.height
            }
            for step in 0..2048i64 {
                let op = random() % 6;
                let key = format!("{:02}", random() % 64);
                if op < 3 || op == 5 {
                    let old = reference.get(&key).copied();
                    let value = match op {
                        0 => None,
                        1 => Some((step, random())),
                        _ => Some((step, old.map_or(0, |old| old.1))),
                    };
                    let next = tree.set(&root, &key, value).expect("set");
                    tree.adjust(&root, -1, 0).expect("release");
                    root = next;
                    match value {
                        Some(value) => reference.insert(key, value),
                        None => reference.remove(&key),
                    };
                } else if op == 3 && cuts.len() < 4 {
                    tree.adjust(&root, 0, 1).expect("pin");
                    cuts.push((root.clone(), reference.clone()));
                } else if op == 4 && !cuts.is_empty() {
                    let index = random() as usize % cuts.len();
                    let (cut, _) = cuts.remove(index);
                    tree.adjust(&cut, 0, -1).expect("unpin");
                }
                if step % 257 == 256 {
                    tree.adjust(&root, -1, 0).expect("drop");
                    root = None;
                    reference.clear();
                }
                let mut seen = HashSet::new();
                for (cut, expected) in std::iter::once((&root, &reference))
                    .chain(cuts.iter().map(|(root, reference)| (root, reference)))
                {
                    let mut keys = Vec::new();
                    inspect(cut, &mut seen, &mut keys);
                    assert_eq!(keys, expected.keys().cloned().collect::<Vec<_>>());
                    let entries: Vec<(String, Value)> =
                        expected.iter().map(|(k, v)| (k.clone(), *v)).collect();
                    assert_eq!(tree.entries(cut, None), entries);
                }
                assert_eq!(tree.resident_nodes(), seen.len() as i64);
            }
            tree.adjust(&root, -1, 0).expect("final");
            for (cut, _) in cuts.drain(..) {
                tree.adjust(&cut, 0, -1).expect("final cut");
            }
            assert_eq!(tree.resident_nodes(), 0);
            assert!(retained.lock().expect("counts").is_empty());
        }
    }
}
