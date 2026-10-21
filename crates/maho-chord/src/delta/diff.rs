use super::{overlap_units, utf16_slice, JsonValue, Op, Path, Seg};
use crate::types::JsString;
use std::collections::HashMap;

const MAX_DELTA_OPERATIONS: usize = 4_096;
const MAX_IDENTITY_CANDIDATES: usize = 200_000;
const MAX_SEMANTIC_CELLS: usize = 65_536;

struct Batch {
    ops: Vec<Op>,
    overflow: bool,
}
impl Batch {
    fn emit(&mut self, op: Op) {
        if self.ops.len() >= MAX_DELTA_OPERATIONS {
            self.overflow = true;
        } else if !self.overflow {
            self.ops.push(op);
        }
    }
    fn set(&mut self, path: &[Seg], value: &JsonValue) {
        self.emit(if path.is_empty() {
            Op::Replace(value.clone())
        } else {
            Op::Set(path.to_vec(), value.clone())
        });
    }
}
pub(crate) fn identity_key(value: &JsonValue) -> String {
    match value {
        JsonValue::Array(v) => format!("a{:p}", std::sync::Arc::as_ptr(v)),
        JsonValue::Object(v) => format!("o{:p}", std::sync::Arc::as_ptr(v)),
        JsonValue::Null => "null".into(),
        JsonValue::Bool(v) => format!("b{v}"),
        JsonValue::Number(v) => format!("n{}", v.as_f64().unwrap_or_default()),
        JsonValue::String(v) => format!("s{:?}", v.units()),
    }
}
fn permutation(before: &[JsonValue], after: &[JsonValue]) -> Option<Vec<usize>> {
    if before.len() != after.len() {
        return None;
    }
    let mut positions: HashMap<String, (Vec<usize>, usize)> = HashMap::new();
    for (index, value) in before.iter().enumerate() {
        positions
            .entry(identity_key(value))
            .or_default()
            .0
            .push(index);
    }
    let mut result = Vec::with_capacity(after.len());
    for value in after {
        let (indices, used) = positions.get_mut(&identity_key(value))?;
        result.push(*indices.get(*used)?);
        *used += 1;
    }
    Some(result)
}
pub(crate) fn emit_string(before: &JsString, after: &JsString, path: &[Seg]) -> Vec<Op> {
    if before == after {
        return Vec::new();
    }
    let before_units = before.units();
    let after_units = after.units();
    if after_units.starts_with(&before_units) {
        return vec![Op::Append(path.to_vec(), after.slice(before_units.len()))];
    }
    let shared = overlap_units(&before_units, &after_units, 65_536, 64, 8);
    if shared == 0 {
        return vec![Op::Set(path.to_vec(), JsonValue::String(after.clone()))];
    }
    let mut operations = vec![Op::Truncate(path.to_vec(), before.len() - shared)];
    if after.len() > shared {
        operations.push(Op::Append(path.to_vec(), utf16_slice(after, shared)));
    }
    operations
}
type ArrayMatch = (usize, usize);
fn semantically_aligned(left: &JsonValue, right: &JsonValue) -> bool {
    if left == right {
        return true;
    }
    match (left, right) {
        (JsonValue::Array(a), JsonValue::Array(b)) => {
            a.len() == b.len()
                && a.iter()
                    .zip(b.iter())
                    .any(|(a, b)| a.is_container() && a.same_identity(b))
        }
        (JsonValue::Object(a), JsonValue::Object(b)) => a.iter().any(|(k, v)| {
            v.is_container() && b.iter().any(|(bk, bv)| k == bk && v.same_identity(bv))
        }),
        _ => false,
    }
}
fn lcs_matches(before: &[JsonValue], after: &[JsonValue]) -> Option<Vec<ArrayMatch>> {
    if before.len().saturating_mul(after.len()) > MAX_SEMANTIC_CELLS {
        return None;
    }
    let width = after.len() + 1;
    let mut lengths = vec![0; (before.len() + 1) * width];
    for left in (0..before.len()).rev() {
        for right in (0..after.len()).rev() {
            lengths[left * width + right] = if semantically_aligned(&before[left], &after[right]) {
                lengths[(left + 1) * width + right + 1] + 1
            } else {
                lengths[(left + 1) * width + right].max(lengths[left * width + right + 1])
            };
        }
    }
    let (mut left, mut right) = (0, 0);
    let mut matches = Vec::new();
    while left < before.len() && right < after.len() {
        if semantically_aligned(&before[left], &after[right])
            && lengths[left * width + right] == lengths[(left + 1) * width + right + 1] + 1
        {
            matches.push((left, right));
            left += 1;
            right += 1;
        } else if lengths[(left + 1) * width + right] >= lengths[left * width + right + 1] {
            left += 1;
        } else {
            right += 1;
        }
    }
    Some(matches)
}
fn identity_subsequence(before: &[JsonValue], after: &[JsonValue]) -> Option<Vec<ArrayMatch>> {
    let mut matches = Vec::new();
    if after.len() < before.len() {
        let mut left = 0;
        for (right, value) in after.iter().enumerate() {
            while left < before.len() && !before[left].same_identity(value) {
                left += 1;
            }
            if left == before.len() {
                return None;
            }
            matches.push((left, right));
            left += 1;
        }
    } else if before.len() < after.len() {
        let mut right = 0;
        for (left, value) in before.iter().enumerate() {
            while right < after.len() && !after[right].same_identity(value) {
                right += 1;
            }
            if right == after.len() {
                return None;
            }
            matches.push((left, right));
            right += 1;
        }
    } else {
        return None;
    }
    Some(matches)
}
fn identity_anchors(before: &[JsonValue], after: &[JsonValue]) -> Vec<ArrayMatch> {
    let mut positions: HashMap<String, Vec<usize>> = HashMap::new();
    for (index, value) in before.iter().enumerate() {
        positions
            .entry(identity_key(value))
            .or_default()
            .push(index);
    }
    let count = after
        .iter()
        .map(|v| positions.get(&identity_key(v)).map_or(0, Vec::len))
        .fold(0usize, usize::saturating_add);
    if count > MAX_IDENTITY_CANDIDATES {
        let mut output = Vec::new();
        let mut minimum = 0;
        for (right, value) in after.iter().enumerate() {
            if let Some(indices) = positions.get(&identity_key(value))
                && let Some(index) = indices.get(indices.partition_point(|i| *i < minimum))
            {
                output.push((*index, right));
                minimum = index + 1;
            }
        }
        return output;
    }
    let mut candidates: Vec<(usize, usize, Option<usize>)> = Vec::new();
    let mut tails = Vec::<usize>::new();
    let mut tail_values = Vec::new();
    for (right, value) in after.iter().enumerate() {
        if let Some(indices) = positions.get(&identity_key(value)) {
            for left in indices.iter().rev() {
                let at = tail_values.partition_point(|v| v < left);
                let previous = at.checked_sub(1).map(|i| tails[i]);
                let candidate = candidates.len();
                candidates.push((*left, right, previous));
                if at == tails.len() {
                    tails.push(candidate);
                    tail_values.push(*left);
                } else {
                    tails[at] = candidate;
                    tail_values[at] = *left;
                }
            }
        }
    }
    let mut output = Vec::new();
    let mut current = tails.last().copied();
    while let Some(index) = current {
        let (left, right, previous) = candidates[index];
        output.push((left, right));
        current = previous;
    }
    output.reverse();
    output
}
fn process_matches(
    before: &[JsonValue],
    after: &[JsonValue],
    path: &[Seg],
    batch: &mut Batch,
    output_start: usize,
    matches: &[ArrayMatch],
) {
    let (mut before_at, mut after_at, mut output_at) = (0, 0, output_start);
    for &(left, right) in matches {
        if batch.overflow {
            return;
        }
        diff_array_region(
            &before[before_at..left],
            &after[after_at..right],
            path,
            batch,
            output_at,
        );
        output_at += right - after_at;
        if before[left] != after[right] {
            let mut child = path.to_vec();
            child.push(Seg::Index(output_at));
            diff_value(&before[left], &after[right], &child, batch);
        }
        output_at += 1;
        before_at = left + 1;
        after_at = right + 1;
    }
    if !batch.overflow {
        diff_array_region(
            &before[before_at..],
            &after[after_at..],
            path,
            batch,
            output_at,
        );
    }
}
fn diff_array_region(
    mut before: &[JsonValue],
    mut after: &[JsonValue],
    path: &[Seg],
    batch: &mut Batch,
    mut output_start: usize,
) {
    if batch.overflow {
        return;
    }
    while !before.is_empty() && !after.is_empty() && before[0] == after[0] {
        before = &before[1..];
        after = &after[1..];
        output_start += 1;
    }
    while !before.is_empty() && !after.is_empty() && before.last() == after.last() {
        before = &before[..before.len() - 1];
        after = &after[..after.len() - 1];
    }
    if before.is_empty() && after.is_empty() {
        return;
    }
    if before.is_empty() || after.is_empty() {
        batch.emit(Op::Splice(
            path.to_vec(),
            output_start,
            before.len(),
            after.to_vec(),
        ));
        return;
    }
    if before.len() == after.len() {
        let positional: Vec<_> = before
            .iter()
            .zip(after)
            .enumerate()
            .filter_map(|(i, (a, b))| (a == b).then_some((i, i)))
            .collect();
        if !positional.is_empty() {
            process_matches(before, after, path, batch, output_start, &positional);
            return;
        }
    }
    if let Some(matches) = identity_subsequence(before, after)
        && !matches.is_empty()
    {
        process_matches(before, after, path, batch, output_start, &matches);
        return;
    }
    let identity = identity_anchors(before, after);
    if !identity.is_empty() {
        process_matches(before, after, path, batch, output_start, &identity);
        return;
    }
    if let Some(matches) = lcs_matches(before, after)
        && !matches.is_empty()
    {
        process_matches(before, after, path, batch, output_start, &matches);
        return;
    }
    if before.len() == 1 && after.len() == 1 {
        let mut child = path.to_vec();
        child.push(Seg::Index(output_start));
        diff_value(&before[0], &after[0], &child, batch);
    } else {
        batch.emit(Op::Splice(
            path.to_vec(),
            output_start,
            before.len(),
            after.to_vec(),
        ));
    }
}
fn diff_value(before: &JsonValue, after: &JsonValue, path: &[Seg], batch: &mut Batch) {
    if before.same_identity(after) || batch.overflow {
        return;
    }
    match (before, after) {
        (JsonValue::String(a), JsonValue::String(b)) if !path.is_empty() => {
            for op in emit_string(a, b, path) {
                batch.emit(op);
            }
        }
        (JsonValue::Array(a), JsonValue::Array(b)) => {
            if a == b {
                return;
            }
            if a.len() == b.len() && a.len() > 1 && a.first() != b.first() && a.last() != b.last()
                && let Some(order) = permutation(a, b)
            {
                batch.emit(Op::Permute(path.to_vec(), order));
                return;
            }
            diff_array_region(a, b, path, batch, 0);
        }
        (JsonValue::Object(a), JsonValue::Object(b)) => {
            if a.iter()
                .chain(b.iter())
                .any(|(k, _)| k.reserved())
            {
                if before != after {
                    batch.set(path, after);
                }
                return;
            }
            for (key, value) in b.iter() {
                if batch.overflow {
                    return;
                }
                let mut child = path.to_vec();
                child.push(Seg::Key(key.clone()));
                if let Some((_, old)) = a.iter().find(|(k, _)| k == key) {
                    diff_value(old, value, &child, batch);
                } else {
                    batch.set(&child, value);
                }
            }
            for (key, _) in a.iter() {
                if !b.iter().any(|(k, _)| k == key) {
                    let mut child = path.to_vec();
                    child.push(Seg::Key(key.clone()));
                    batch.emit(Op::Delete(child));
                }
            }
        }
        _ => batch.set(path, after),
    }
}
pub(crate) fn json_cost(value: &JsonValue) -> usize {
    match value {
        JsonValue::Null => 4,
        JsonValue::Bool(v) => {
            if *v {
                4
            } else {
                5
            }
        }
        JsonValue::Number(v) => v.to_string().len(),
        JsonValue::String(v) => v.len() + 2,
        JsonValue::Array(v) => {
            2 + v.iter().map(json_cost).sum::<usize>() + v.len().saturating_sub(1)
        }
        JsonValue::Object(v) => {
            2 + v
                .iter()
                .map(|(k, v)| k.len() + 3 + json_cost(v))
                .sum::<usize>()
                + v.len().saturating_sub(1)
        }
    }
}
fn path_cost(path: &[Seg]) -> usize {
    2 + path
        .iter()
        .map(|seg| match seg {
            Seg::Key(k) => k.len() + 2,
            Seg::Index(i) => i.to_string().len(),
        })
        .sum::<usize>()
        + path.len().saturating_sub(1)
}
fn operation_cost(op: &Op) -> usize {
    match op {
        Op::Replace(v) => 6 + json_cost(v),
        Op::Set(p, v) => 7 + path_cost(p) + json_cost(v),
        Op::Delete(p) => 6 + path_cost(p),
        Op::Append(p, v) => 9 + path_cost(p) + v.len(),
        Op::Truncate(p, n) => 7 + path_cost(p) + n.to_string().len(),
        Op::Splice(p, i, r, v) => {
            10 + path_cost(p)
                + i.to_string().len()
                + r.to_string().len()
                + json_cost(&JsonValue::array(v.clone()))
        }
        Op::Permute(p, order) => {
            9 + path_cost(p)
                + order.iter().map(|i| i.to_string().len()).sum::<usize>()
                + order.len().saturating_sub(1)
        }
    }
}
pub fn diff_revisions(before: &JsonValue, after: &JsonValue) -> Vec<Op> {
    let mut batch = Batch {
        ops: Vec::new(),
        overflow: false,
    };
    diff_value(before, after, &Path::new(), &mut batch);
    if batch.overflow {
        return vec![Op::Replace(after.clone())];
    }
    if batch.ops.is_empty() || matches!(batch.ops.first(), Some(Op::Replace(_))) {
        return batch.ops;
    }
    let cost = 2 + batch
        .ops
        .iter()
        .map(|op| operation_cost(op) + 1)
        .sum::<usize>();
    if cost >= 65_536 && cost >= json_cost(after) + 6 {
        vec![Op::Replace(after.clone())]
    } else {
        batch.ops
    }
}
