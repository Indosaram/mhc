//! Lenient, smallest-subtree validation of one layer and of the merged view (spec §4.4, §6.4).

use crate::diagnostics::{Diagnostic, DiagnosticKind, DiagnosticTarget};
use crate::legacy_names::{rename_legacy_categories, rename_legacy_harness};
use crate::model_string::normalize_document;
use crate::schema::{self, IssueKind, Mode, Seg, dotted, get_path, remove_path, strip_unsafe_keys};
use serde_json::Value;

/// Maximum number of prune passes before a document is rejected.
pub const MAX_PRUNE_PASSES: usize = 32;

/// One value removed by pruning.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pruned {
    pub key: String,
    pub message: String,
}

/// Why pruning could not produce a valid document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rejected {
    /// Dotted paths of the issues that remained.
    pub issues: Vec<String>,
}

fn has_ancestor_in(path: &[Seg], set: &[Vec<Seg>]) -> bool {
    set.iter()
        .any(|other| other.len() < path.len() && path.starts_with(other))
}

/// Deletes the smallest failing subtrees until `doc` validates in `mode`.
pub fn prune_until_valid(doc: &mut Value, mode: Mode) -> Result<Vec<Pruned>, Rejected> {
    let mut pruned = Vec::new();
    for _ in 0..MAX_PRUNE_PASSES {
        let issues = schema::validate(doc, mode);
        if issues.is_empty() {
            return Ok(pruned);
        }
        let paths: Vec<String> = issues.iter().map(|issue| dotted(&issue.path)).collect();
        let mut targets: Vec<(Vec<Seg>, String)> = Vec::new();
        for issue in &issues {
            if issue.path.is_empty() {
                return Err(Rejected { issues: paths });
            }
            let target = if get_path(doc, &issue.path).is_some() {
                issue.path.clone()
            } else {
                let mut ancestor = issue.path.clone();
                while !ancestor.is_empty()
                    && !get_path(doc, &ancestor).is_some_and(Value::is_object)
                {
                    ancestor.pop();
                }
                ancestor
            };
            if target.is_empty() {
                return Err(Rejected { issues: paths });
            }
            if !targets.iter().any(|(existing, _)| *existing == target) {
                targets.push((target, issue.message.clone()));
            }
        }
        let all: Vec<Vec<Seg>> = targets.iter().map(|(path, _)| path.clone()).collect();
        targets.retain(|(path, _)| !has_ancestor_in(path, &all));
        targets.sort_by(|a, b| b.0.cmp(&a.0));
        for (target, message) in targets {
            if remove_path(doc, &target) {
                remove_emptied_ancestors(doc, &target);
                pruned.push(Pruned {
                    key: dotted(&target),
                    message,
                });
            }
        }
        // "Nothing valid is left": every value of the document was invalid. A document whose
        // single value was invalid is still loaded (as an empty layer) so the invalid value is
        // reported as such (spec §17 cases 16 and 34).
        if schema::is_empty_object(doc) && pruned.len() > 1 {
            return Err(Rejected { issues: paths });
        }
    }
    let issues = schema::validate(doc, mode)
        .iter()
        .map(|issue| dotted(&issue.path))
        .collect();
    Err(Rejected { issues })
}

fn remove_emptied_ancestors(doc: &mut Value, removed: &[Seg]) {
    let mut parent = removed[..removed.len() - 1].to_vec();
    while !parent.is_empty() {
        let empty = match get_path(doc, &parent) {
            Some(Value::Object(map)) => map.is_empty(),
            Some(Value::Array(items)) => items.is_empty(),
            _ => false,
        };
        if !empty {
            return;
        }
        remove_path(doc, &parent);
        parent.pop();
    }
}

/// Outcome of validating one layer.
#[derive(Clone, Debug, Default)]
pub struct LayerOutcome {
    /// The canonicalised layer, or `None` when the file was rejected.
    pub value: Option<Value>,
    pub diagnostics: Vec<Diagnostic>,
}

/// Validates one parsed layer (§4.4), folding legacy fields (§4.5) and canonicalising legacy
/// names (§4.6). `shown` is the display form of the file path used inside messages.
pub fn validate_layer(raw: Option<Value>, target: DiagnosticTarget, shown: &str) -> LayerOutcome {
    let mut diagnostics = Vec::new();
    let Some(mut value) = raw else {
        diagnostics.push(Diagnostic::new(
            DiagnosticKind::Validation,
            target,
            format!("{shown}: not loaded: the root must be an object"),
            Vec::new(),
        ));
        return LayerOutcome {
            value: None,
            diagnostics,
        };
    };
    let unsafe_paths = strip_unsafe_keys(&mut value);
    if !unsafe_paths.is_empty() {
        let keys: Vec<String> = unsafe_paths.iter().map(|path| dotted(path)).collect();
        diagnostics.push(Diagnostic::new(
            DiagnosticKind::UnknownKeys,
            target.clone(),
            format!("{shown}: removed unsafe key(s): {}", keys.join(", ")),
            keys,
        ));
    }
    if !value.is_object() {
        diagnostics.push(Diagnostic::new(
            DiagnosticKind::Validation,
            target,
            format!("{shown}: not loaded: the root must be an object"),
            Vec::new(),
        ));
        return LayerOutcome {
            value: None,
            diagnostics,
        };
    }
    normalize_document(&mut value);
    let issues = schema::validate(&mut value, Mode::Layer);
    let unknown: Vec<Vec<Seg>> = issues
        .iter()
        .filter(|issue| issue.kind == IssueKind::UnknownKey)
        .map(|issue| issue.path.clone())
        .collect();
    if !unknown.is_empty() {
        let mut ordered = unknown.clone();
        ordered.sort_by(|a, b| b.cmp(a));
        for path in &ordered {
            remove_path(&mut value, path);
        }
        let keys: Vec<String> = unknown.iter().map(|path| dotted(path)).collect();
        diagnostics.push(Diagnostic::new(
            DiagnosticKind::UnknownKeys,
            target.clone(),
            format!("{shown}: ignored unknown key(s): {}", keys.join(", ")),
            keys,
        ));
    }
    match prune_until_valid(&mut value, Mode::Layer) {
        Ok(pruned) => {
            for item in pruned {
                diagnostics.push(Diagnostic::new(
                    DiagnosticKind::InvalidValue,
                    target.clone(),
                    format!(
                        "{shown}: ignored invalid value at {} ({})",
                        item.key, item.message
                    ),
                    vec![item.key],
                ));
            }
        }
        Err(rejected) => {
            diagnostics.push(Diagnostic::new(
                DiagnosticKind::Validation,
                target,
                format!(
                    "{shown}: not loaded: invalid value(s) at {}",
                    rejected.issues.join(", ")
                ),
                rejected.issues,
            ));
            return LayerOutcome {
                value: None,
                diagnostics,
            };
        }
    }
    let mut renames = rename_legacy_categories(&mut value);
    renames.extend(rename_legacy_harness(&mut value));
    if !renames.is_empty() {
        let parts: Vec<String> = renames
            .iter()
            .map(|rename| {
                if rename.renamed {
                    format!("{} (renamed)", rename.path)
                } else {
                    format!("{} (ignored, the new name is already set)", rename.path)
                }
            })
            .collect();
        diagnostics.push(Diagnostic::new(
            DiagnosticKind::DeprecatedKeys,
            target,
            format!("{shown}: deprecated name(s): {}", parts.join(", ")),
            renames.into_iter().map(|rename| rename.path).collect(),
        ));
    }
    LayerOutcome {
        value: Some(value),
        diagnostics,
    }
}
