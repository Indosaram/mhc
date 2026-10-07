//! Bounded surgical pruning of invalid config values.
//!
//! Port of `packages/omo-config-core/src/loader/prune-invalid-leaves.ts`. One bad value must not
//! take down the file that carries it: every validation issue names a path, and the loader drops
//! only the smallest subtree that fails (the value at that path, or the object that lacks a
//! required key), keeps every healthy sibling at every depth, and re-validates after each pass
//! until the document parses. A container left empty because every child was dropped goes with
//! them, so a document with nothing valid left comes back empty. If the pass bound is hit, or an
//! issue targets the document root, the caller rejects the whole layer fail-closed (old totality
//! preserved as the last resort, never the first).

use std::collections::HashSet;

use serde_json::{Map, Value};

use crate::issue::{Issue, IssueCode, Issues};

/// A dropped value: its dotted key (`task.host_engine_policy`) plus the issue message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrunedConfigPath {
    pub key: String,
    pub message: String,
    pub path: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PruneResult {
    Ok {
        config: Map<String, Value>,
        dropped: Vec<PrunedConfigPath>,
    },
    NotOk {
        dropped: Vec<PrunedConfigPath>,
    },
}

pub type PruneValidator<'a> = dyn Fn(&Map<String, Value>) -> Result<(), Issues> + 'a;

/// Each pass drops every path the current issues name, so a real document settles in a few passes.
pub const MAX_PRUNE_PASSES: usize = 32;

fn child_of<'a>(container: &'a Value, segment: &str) -> Option<&'a Value> {
    match container {
        Value::Array(items) => segment
            .parse::<usize>()
            .ok()
            .and_then(|index| items.get(index)),
        Value::Object(map) => map.get(segment),
        _ => None,
    }
}

/// The value at `path`, walking through array elements (`teams.alpha.members.0`).
pub(crate) fn value_at_mut<'a>(node: &'a mut Value, path: &[String]) -> Option<&'a mut Value> {
    match path.split_first() {
        None => Some(node),
        Some((segment, rest)) => {
            let child = match node {
                Value::Object(map) => map.get_mut(segment)?,
                Value::Array(items) => {
                    let index = segment.parse::<usize>().ok()?;
                    items.get_mut(index)?
                }
                _ => return None,
            };
            value_at_mut(child, rest)
        }
    }
}

/// The smallest subtree an issue condemns: the deepest prefix of its path that exists in the
/// document. A wrong value resolves to its own path; a missing required key resolves to the object
/// that lacks it. An empty result means the issue is about the document root itself.
fn target_paths(root: &Map<String, Value>, issue: &Issue) -> Vec<Vec<String>> {
    if issue.code == IssueCode::UnrecognizedKeys {
        return issue
            .keys
            .iter()
            .map(|key| {
                let mut path = issue.path.clone();
                path.push(key.clone());
                path
            })
            .collect();
    }
    let base = &issue.path;
    let mut node: Option<&Value> = None;
    let mut depth = 0;
    for segment in base {
        let next = match node {
            None => root.get(segment),
            Some(value) => child_of(value, segment),
        };
        match next {
            Some(value) => {
                node = Some(value);
                depth += 1;
            }
            None => break,
        }
    }
    vec![base[..depth].to_vec()]
}

fn is_ancestor(ancestor: &[String], path: &[String]) -> bool {
    ancestor.len() < path.len() && ancestor.iter().zip(path.iter()).all(|(a, b)| a == b)
}

// Later array indices first, so removing one element never shifts a path still waiting to be removed.
fn compare_descending(left: &[String], right: &[String]) -> std::cmp::Ordering {
    let length = left.len().min(right.len());
    for index in 0..length {
        let a = &left[index];
        let b = &right[index];
        if a == b {
            continue;
        }
        return match (a.parse::<i64>(), b.parse::<i64>()) {
            (Ok(left_number), Ok(right_number)) => right_number.cmp(&left_number),
            _ => b.cmp(a),
        };
    }
    right.len().cmp(&left.len())
}

fn remove_child(container: &mut Value, segment: &str) {
    match container {
        Value::Object(map) => {
            map.remove(segment);
        }
        Value::Array(items) => {
            if let Ok(index) = segment.parse::<usize>()
                && index < items.len()
            {
                items.remove(index);
            }
        }
        _ => {}
    }
}

fn is_empty_container(container: &Value) -> bool {
    match container {
        Value::Object(map) => map.is_empty(),
        Value::Array(items) => items.is_empty(),
        _ => false,
    }
}

fn remove_path_and_emptied_ancestors(root: &mut Value, path: &[String]) {
    for depth in (0..path.len()).rev() {
        let Some(container) = value_at_mut(root, &path[..depth]) else {
            return;
        };
        remove_child(container, &path[depth]);
        if depth == 0 || !is_empty_container(container) {
            return;
        }
    }
}

fn prune_pass(
    root: &Map<String, Value>,
    issues: &[Issue],
) -> Option<(Vec<PrunedConfigPath>, Map<String, Value>)> {
    let mut targets: Vec<PrunedConfigPath> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for issue in issues {
        for path in target_paths(root, issue) {
            if path.is_empty() {
                return None;
            }
            let key = path.join(".");
            if seen.insert(key.clone()) {
                targets.push(PrunedConfigPath {
                    key,
                    message: issue.message.clone(),
                    path,
                });
            }
        }
    }
    let mut outermost: Vec<PrunedConfigPath> = targets
        .iter()
        .filter(|target| {
            !targets
                .iter()
                .any(|other| is_ancestor(&other.path, &target.path))
        })
        .cloned()
        .collect();
    outermost.sort_by(|left, right| compare_descending(&left.path, &right.path));
    let mut next = Value::Object(root.clone());
    for target in &outermost {
        remove_path_and_emptied_ancestors(&mut next, &target.path);
    }
    match next {
        Value::Object(map) => Some((outermost, map)),
        _ => Some((outermost, Map::new())),
    }
}

/// Prune every invalid value out of `config`, re-validating through `validate` after each pass.
/// `issues` are the validation issues of `config` as given. The result is `NotOk` when the bound
/// is exhausted, an issue targets the document root, or nothing valid is left; the caller then
/// rejects the layer fail-closed.
pub fn prune_invalid_config_paths(
    config: &Map<String, Value>,
    issues: &[Issue],
    validate: &PruneValidator<'_>,
    max_passes: usize,
) -> PruneResult {
    let mut dropped: Vec<PrunedConfigPath> = Vec::new();
    let mut current = config.clone();
    let mut pending: Vec<Issue> = issues.to_vec();
    for _ in 0..max_passes {
        let Some((step_dropped, next)) = prune_pass(&current, &pending) else {
            return PruneResult::NotOk { dropped };
        };
        if step_dropped.is_empty() {
            return PruneResult::NotOk { dropped };
        }
        dropped.extend(step_dropped);
        current = next;
        if current.is_empty() {
            return PruneResult::NotOk { dropped };
        }
        match validate(&current) {
            Ok(()) => return PruneResult::Ok { config: current, dropped },
            Err(next_issues) => pending = next_issues,
        }
    }
    // Bound exhausted and still failing: signal the caller to reject the layer.
    PruneResult::NotOk { dropped }
}
