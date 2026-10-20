use crate::types::JsString;
pub use crate::types::JsonValue;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

pub mod apply_immutable_trusted;
pub mod diff;
pub mod revision_validator;
pub mod tracker;
pub use diff::diff_revisions;
pub use tracker::{track, Change, Draft, Prepared, Tracker};

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(untagged)]
pub enum Seg {
    Key(JsString),
    Index(usize),
}
impl<'de> Deserialize<'de> for Seg {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let value = JsonValue::deserialize(d)?;
        match value {
            JsonValue::String(key) => Ok(Self::Key(key)),
            value => count(&value).map(Self::Index).map_err(serde::de::Error::custom),
        }
    }
}
impl From<&str> for Seg {
    fn from(value: &str) -> Self {
        Self::Key(value.into())
    }
}
impl From<usize> for Seg {
    fn from(value: usize) -> Self {
        Self::Index(value)
    }
}
impl From<String> for Seg {
    fn from(value: String) -> Self { Self::Key(value.into()) }
}
impl From<JsString> for Seg {
    fn from(value: JsString) -> Self { Self::Key(value) }
}
fn path_value(path: &[Seg]) -> JsonValue {
    JsonValue::array(path.iter().map(|seg| match seg {
        Seg::Key(key) => JsonValue::String(key.clone()),
        Seg::Index(index) => json!(index).into(),
    }).collect())
}
pub type Path = Vec<Seg>;
pub type WireOp = JsonValue;

#[derive(Clone, Debug, PartialEq)]
pub enum Op {
    Replace(JsonValue),
    Set(Path, JsonValue),
    Delete(Path),
    Append(Path, JsString),
    Truncate(Path, usize),
    Splice(Path, usize, usize, Vec<JsonValue>),
    Permute(Path, Vec<usize>),
}
impl Op {
    pub fn path(&self) -> Option<&[Seg]> {
        match self {
            Self::Replace(_) => None,
            Self::Set(p, _)
            | Self::Delete(p)
            | Self::Append(p, _)
            | Self::Truncate(p, _)
            | Self::Splice(p, _, _, _)
            | Self::Permute(p, _) => Some(p),
        }
    }
    pub fn to_tuple(&self) -> JsonValue {
        let path = |p: &Path| path_value(p);
        let verb = |v: &str| JsonValue::String(v.into());
        JsonValue::array(match self {
            Self::Replace(v) => vec![verb("r"), v.clone()],
            Self::Set(p, v) => vec![verb("s"), path(p), v.clone()],
            Self::Delete(p) => vec![verb("d"), path(p)],
            Self::Append(p, v) => vec![verb("a"), path(p), JsonValue::String(v.clone())],
            Self::Truncate(p, n) => vec![verb("t"), path(p), json!(n).into()],
            Self::Splice(p, i, r, v) => vec![
                verb("p"),
                path(p),
                json!(i).into(),
                json!(r).into(),
                JsonValue::array(v.clone()),
            ],
            Self::Permute(p, v) => vec![verb("m"), path(p), json!(v).into()],
        })
    }
}
impl Serialize for Op {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        self.to_tuple().serialize(s)
    }
}
impl<'de> Deserialize<'de> for Op {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        valid_op(&JsonValue::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DeltaError {
    #[error("unsafe path segment: {0:?}")]
    UnsafePath(Seg),
    #[error("unresolvable path: {0:?}")]
    Path(Path),
    #[error("unresolvable path id: {0}")]
    PathId(usize),
    #[error("invalid operation: {0}")]
    Shape(&'static str),
    #[error("change has already been settled")]
    Settled,
    #[error("prepared change belongs to a different tracker")]
    Foreign,
    #[error("prepared change has already been used")]
    Consumed,
    #[error("prepared change has been aborted")]
    Aborted,
    #[error("prepared change is stale")]
    Stale,
    #[error("tracked root must be an object or array")]
    Root,
}
pub type Result<T> = std::result::Result<T, DeltaError>;
pub const RESERVED_SEGMENTS: [&str; 3] = ["__proto__", "constructor", "prototype"];
pub fn is_replace(op: &Op) -> bool {
    matches!(op, Op::Replace(_))
}
pub fn is_base(ops: &[Op]) -> bool {
    ops.first().is_some_and(is_replace)
}
pub fn assert_safe_path(path: &[Seg]) -> Result<()> {
    for seg in path {
        if let Seg::Key(key) = seg && key.reserved() {
            return Err(DeltaError::UnsafePath(seg.clone()));
        }
    }
    Ok(())
}
fn parse_path(value: &JsonValue, nonempty: bool) -> Result<Path> {
    let path: Path = value.as_array().ok_or(DeltaError::Shape("path"))?.iter()
        .map(|value| match value {
            JsonValue::String(key) => Ok(Seg::Key(key.clone())),
            _ => count(value).map(Seg::Index),
        }).collect::<Result<_>>()?;
    if nonempty && path.is_empty() {
        return Err(DeltaError::Shape("empty path"));
    }
    assert_safe_path(&path)?;
    Ok(path)
}
fn count(value: &JsonValue) -> Result<usize> {
    if let Some(v) = value.as_u64().and_then(|v| usize::try_from(v).ok()) {
        return Ok(v);
    }
    Err(DeltaError::Shape("nonnegative integer"))
}
fn permutation(value: &JsonValue) -> Result<Vec<usize>> {
    let values = value.as_array().ok_or(DeltaError::Shape("permutation"))?;
    let mut seen = vec![false; values.len()];
    let mut output = Vec::with_capacity(values.len());
    for value in values {
        let index = count(value)?;
        if index >= seen.len() || seen[index] {
            return Err(DeltaError::Shape("permutation is not a bijection"));
        }
        seen[index] = true;
        output.push(index);
    }
    Ok(output)
}
pub fn assert_valid_op<T: Clone + Into<JsonValue>>(value: &T) -> Result<Op> {
    valid_op(&value.clone().into())
}
pub fn valid_op(value: &JsonValue) -> Result<Op> {
    let op = value.as_array().ok_or(DeltaError::Shape("tuple"))?;
    let verb = op
        .first()
        .and_then(JsonValue::as_str)
        .ok_or(DeltaError::Shape("verb"))?;
    match (verb, op.len()) {
        ("r", 2) => Ok(Op::Replace(op[1].clone())),
        ("s", 3) => Ok(Op::Set(parse_path(&op[1], true)?, op[2].clone())),
        ("d", 2) => Ok(Op::Delete(parse_path(&op[1], true)?)),
        ("a", 3) => Ok(Op::Append(
            parse_path(&op[1], true)?,
            match &op[2] {
                JsonValue::String(value) => value.clone(),
                _ => return Err(DeltaError::Shape("append string")),
            },
        )),
        ("t", 3) => Ok(Op::Truncate(parse_path(&op[1], true)?, count(&op[2])?)),
        ("p", 5) => Ok(Op::Splice(
            parse_path(&op[1], false)?,
            count(&op[2])?,
            count(&op[3])?,
            op[4]
                .as_array()
                .ok_or(DeltaError::Shape("splice items"))?
                .to_vec(),
        )),
        ("m", 3) => Ok(Op::Permute(
            parse_path(&op[1], false)?,
            permutation(&op[2])?,
        )),
        _ => Err(DeltaError::Shape("verb or arity")),
    }
}
pub fn assert_valid_wire_op<T: Clone + Into<JsonValue>>(value: &T) -> Result<()> {
    valid_wire_op(&value.clone().into())
}
pub fn valid_wire_op(value: &JsonValue) -> Result<()> {
    let op = value.as_array().ok_or(DeltaError::Shape("tuple"))?;
    let verb = op
        .first()
        .and_then(JsonValue::as_str)
        .ok_or(DeltaError::Shape("verb"))?;
    if verb == "#" {
        if op.len() != 3 {
            return Err(DeltaError::Shape("definition arity"));
        }
        count(&op[1])?;
        parse_path(&op[2], false)?;
        return Ok(());
    }
    if verb == "r" {
        valid_op(value)?;
        return Ok(());
    }
    let full = match verb {
        "d" => 2,
        "s" | "a" | "t" | "m" => 3,
        "p" => 5,
        _ => return Err(DeltaError::Shape("verb")),
    };
    let mut expanded = op.clone();
    if op.len() == full {
        if op[1].is_number() {
            count(&op[1])?;
        } else {
            parse_path(&op[1], false)?;
        }
        expanded[1] = json!(["wire"]).into();
    } else if op.len() + 1 == full {
        expanded.insert(1, json!(["wire"]).into());
    } else {
        return Err(DeltaError::Shape("wire arity"));
    }
    valid_op(&JsonValue::array(expanded))?;
    Ok(())
}

pub fn overlap(a: &str, b: &str, scan: usize, probe: usize, max_candidates: usize) -> usize {
    overlap_units(
        &a.encode_utf16().collect::<Vec<_>>(),
        &b.encode_utf16().collect::<Vec<_>>(),
        scan,
        probe,
        max_candidates,
    )
}
pub(crate) fn overlap_units(
    a: &[u16],
    b: &[u16],
    scan: usize,
    probe: usize,
    max_candidates: usize,
) -> usize {
    if a.is_empty() || b.is_empty() || scan == 0 {
        return 0;
    }
    let tail = &a[a.len().saturating_sub(scan)..];
    for h in [probe.min(b.len()).max(1), 1] {
        let mut tried = 0;
        for k in 0..=tail.len().saturating_sub(h) {
            if tail.get(k..k + h) != Some(&b[..h]) {
                continue;
            }
            tried += 1;
            if tried > max_candidates {
                break;
            }
            let n = tail.len() - k;
            if n <= b.len() && tail[k..] == b[..n] {
                return n;
            }
        }
        if h == 1 {
            break;
        }
    }
    0
}
pub(crate) fn utf16_slice(value: &JsString, start: usize) -> JsString {
    value.slice(start)
}
pub fn resolve_value<'a>(root: &'a JsonValue, path: &[Seg]) -> Result<&'a JsonValue> {
    let mut node = root;
    for seg in path {
        node = match (node, seg) {
            (JsonValue::Array(values), Seg::Index(i)) => values.get(*i),
            (JsonValue::Array(_), Seg::Key(_)) => return Err(DeltaError::UnsafePath(seg.clone())),
            (JsonValue::Object(values), seg) => {
                let key = key(seg);
                values.iter().find(|(k, _)| *k == key).map(|(_, v)| v)
            }
            _ => None,
        }
        .ok_or_else(|| DeltaError::Path(path.to_vec()))?;
    }
    Ok(node)
}
pub(crate) fn key(seg: &Seg) -> JsString {
    match seg {
        Seg::Key(k) => k.clone(),
        Seg::Index(i) => i.to_string().into(),
    }
}
pub(crate) fn resolve_mut<'a>(root: &'a mut JsonValue, path: &[Seg]) -> Result<&'a mut JsonValue> {
    let mut node = root;
    for seg in path {
        node = match (node, seg) {
            (JsonValue::Array(values), Seg::Index(i)) => Arc::make_mut(values).get_mut(*i),
            (JsonValue::Array(_), Seg::Key(_)) => return Err(DeltaError::UnsafePath(seg.clone())),
            (JsonValue::Object(values), seg) => {
                let key = key(seg);
                Arc::make_mut(values)
                    .iter_mut()
                    .find(|(k, _)| *k == key)
                    .map(|(_, v)| v)
            }
            _ => None,
        }
        .ok_or_else(|| DeltaError::Path(path.to_vec()))?;
    }
    Ok(node)
}
pub(crate) fn apply_one(root: &mut JsonValue, op: &Op, checked: bool) -> Result<()> {
    if let Op::Replace(value) = op {
        *root = value.clone();
        return Ok(());
    }
    let path = op.path().ok_or(DeltaError::Shape("path"))?;
    if checked {
        assert_safe_path(path)?;
    }
    match op {
        Op::Replace(_) => unreachable!(),
        Op::Splice(_, start, remove, items) => {
            let JsonValue::Array(values) = resolve_mut(root, path)? else {
                return Err(DeltaError::Path(path.to_vec()));
            };
            let values = Arc::make_mut(values);
            let start = (*start).min(values.len());
            let end = start.saturating_add(*remove).min(values.len());
            values.splice(start..end, items.iter().cloned());
        }
        Op::Permute(_, order) => {
            let JsonValue::Array(values) = resolve_mut(root, path)? else {
                return Err(DeltaError::Path(path.to_vec()));
            };
            if values.len() != order.len() {
                return Err(DeltaError::Path(path.to_vec()));
            }
            let mut seen = vec![false; order.len()];
            for i in order {
                if *i >= order.len() || seen[*i] {
                    return Err(DeltaError::Shape("permutation is not a bijection"));
                }
                seen[*i] = true;
            }
            let previous = values.clone();
            *values = Arc::new(order.iter().map(|i| previous[*i].clone()).collect());
        }
        Op::Set(_, _) | Op::Delete(_) | Op::Append(_, _) | Op::Truncate(_, _) => {
            let (last, parent_path) = path.split_last().ok_or(DeltaError::Shape("empty path"))?;
            let parent = resolve_mut(root, parent_path)?;
            let next = match op {
                Op::Set(_, v) => Some(v.clone()),
                Op::Delete(_) => None,
                Op::Append(_, suffix) => {
                    let JsonValue::String(v) = resolve_value(parent, std::slice::from_ref(last))?
                    else {
                        return Err(DeltaError::Path(path.to_vec()));
                    };
                    Some(JsonValue::String(v.concat(suffix)))
                }
                Op::Truncate(_, n) => {
                    let JsonValue::String(v) = resolve_value(parent, std::slice::from_ref(last))?
                    else {
                        return Err(DeltaError::Path(path.to_vec()));
                    };
                    Some(JsonValue::String(utf16_slice(v, *n)))
                }
                Op::Replace(_) | Op::Splice(_, _, _, _) | Op::Permute(_, _) => unreachable!(),
            };
            match parent {
                JsonValue::Array(values) => {
                    let Seg::Index(i) = last else {
                        return Err(DeltaError::UnsafePath(last.clone()));
                    };
                    if *i > values.len() {
                        return Err(DeltaError::UnsafePath(last.clone()));
                    }
                    let values = Arc::make_mut(values);
                    if let Some(next) = next {
                        if *i == values.len() {
                            values.push(next);
                        } else {
                            values[*i] = next;
                        }
                    } else {
                        if *i == values.len() {
                            return Err(DeltaError::Path(path.to_vec()));
                        }
                        values.remove(*i);
                    }
                }
                JsonValue::Object(values) => {
                    let k = key(last);
                    let values = Arc::make_mut(values);
                    let at = values.iter().position(|(key, _)| *key == k);
                    match (at, next) {
                        (Some(i), Some(v)) => values[i].1 = v,
                        (None, Some(v)) => values.push((k, v)),
                        (Some(i), None) => {
                            values.remove(i);
                        }
                        (None, None) => {}
                    }
                    values.sort_by(|(a, _), (b, _)| {
                        let index = |key: &JsString| key.array_index();
                        match (index(a), index(b)) {
                            (Some(a), Some(b)) => a.cmp(&b),
                            (Some(_), None) => std::cmp::Ordering::Less,
                            (None, Some(_)) => std::cmp::Ordering::Greater,
                            (None, None) => std::cmp::Ordering::Equal,
                        }
                    });
                }
                JsonValue::Null
                | JsonValue::Bool(_)
                | JsonValue::Number(_)
                | JsonValue::String(_) => return Err(DeltaError::Path(path.to_vec())),
            }
        }
    }
    Ok(())
}
pub fn apply(mut target: Option<JsonValue>, ops: &[Op]) -> Result<JsonValue> {
    for op in ops {
        if let Op::Replace(value) = op {
            target = Some(value.clone());
        } else {
            apply_one(
                target.as_mut().ok_or_else(|| DeltaError::Path(vec![]))?,
                op,
                true,
            )?;
        }
    }
    target.ok_or_else(|| DeltaError::Path(vec![]))
}
pub fn apply_immutable(target: Option<&JsonValue>, ops: &[Op]) -> Result<JsonValue> {
    apply(target.cloned(), ops)
}
pub fn apply_immutable_batches<'a>(
    target: Option<&JsonValue>,
    batches: impl IntoIterator<Item = &'a [Op]>,
) -> Result<JsonValue> {
    let mut root = target.cloned();
    for ops in batches {
        root = Some(apply(root, ops)?);
    }
    root.ok_or_else(|| DeltaError::Path(vec![]))
}

#[derive(Default)]
pub struct Encoder {
    seen: HashSet<Path>,
    ids: HashMap<Path, usize>,
    next_id: usize,
}
pub fn encoder() -> Encoder {
    Encoder::default()
}
impl Encoder {
    pub fn encode(&mut self, ops: &[Op]) -> Vec<WireOp> {
        let mut previous: Option<Path> = None;
        let mut out = Vec::new();
        for op in ops {
            let Some(path) = op.path() else {
                out.push(op.to_tuple());
                self.seen.clear();
                self.ids.clear();
                self.next_id = 0;
                previous = None;
                continue;
            };
            let mut tuple = match op.to_tuple() {
                JsonValue::Array(v) => v.as_ref().clone(),
                _ => unreachable!(),
            };
            if previous.as_deref() == Some(path) {
                tuple.remove(1);
            } else {
                if let Some(id) = self.ids.get(path) {
                    tuple[1] = json!(id).into();
                } else if self.seen.contains(path) {
                    let id = self.next_id;
                    self.next_id += 1;
                    self.ids.insert(path.to_vec(), id);
                    out.push(JsonValue::array(vec![JsonValue::String("#".into()), json!(id).into(), path_value(path)]));
                    tuple[1] = json!(id).into();
                } else {
                    self.seen.insert(path.to_vec());
                }
                previous = Some(path.to_vec());
            }
            out.push(JsonValue::array(tuple));
        }
        out
    }
}
#[derive(Default)]
pub struct Decoder {
    paths: HashMap<usize, Path>,
}
pub fn decoder() -> Decoder {
    Decoder::default()
}
impl Decoder {
    pub fn decode<T: Clone + Into<JsonValue>>(&mut self, wire: &[T]) -> Result<Vec<Op>> {
        let mut previous: Option<Path> = None;
        let mut out = Vec::new();
        for value in wire {
            let value: JsonValue = value.clone().into();
            valid_wire_op(&value)?;
            let tuple = value.as_array().ok_or(DeltaError::Shape("tuple"))?;
            let verb = tuple[0].as_str().ok_or(DeltaError::Shape("verb"))?;
            if verb == "#" {
                self.paths
                    .insert(count(&tuple[1])?, parse_path(&tuple[2], false)?);
                continue;
            }
            if verb == "r" {
                out.push(valid_op(&value)?);
                self.paths.clear();
                previous = None;
                continue;
            }
            let short = match verb {
                "d" => tuple.len() == 1,
                "p" => tuple.len() == 4,
                _ => tuple.len() == 2,
            };
            let path = if short {
                previous.clone().ok_or_else(|| DeltaError::Path(vec![]))?
            } else {
                let path = if tuple[1].is_number() {
                    let id = count(&tuple[1])?;
                    self.paths.get(&id).cloned().ok_or(DeltaError::PathId(id))?
                } else {
                    parse_path(&tuple[1], false)?
                };
                previous = Some(path.clone());
                path
            };
            let mut expanded = tuple.clone();
            if short {
                expanded.insert(1, path_value(&path));
            } else {
                expanded[1] = path_value(&path);
            }
            out.push(valid_op(&JsonValue::array(expanded))?);
        }
        Ok(out)
    }
}
