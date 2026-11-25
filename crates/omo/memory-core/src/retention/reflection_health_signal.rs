//! Reflection-health gate for discarding unreflected journals (design 2.3).

use std::path::Path;

use serde_json::Value;

use crate::support::time::{format_rfc3339_millis, parse_rfc3339};

use super::policy::list_names;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReflectionFailureKind {
    Failed,
    Abandoned,
    Quarantined,
    Parked,
}

impl ReflectionFailureKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Failed => "failed",
            Self::Abandoned => "abandoned",
            Self::Quarantined => "quarantined",
            Self::Parked => "parked",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateNewestFailure {
    pub kind: ReflectionFailureKind,
    pub at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnreflectedRetentionGate {
    pub open: bool,
    pub newest: Option<GateNewestFailure>,
}

fn read_json(path: &Path) -> Option<Value> {
    let raw = std::fs::read_to_string(path).ok()?;
    serde_json::from_str::<Value>(&raw).ok()
}

fn string_field<'a>(value: Option<&'a Value>, key: &str) -> Option<&'a str> {
    value?
        .as_object()?
        .get(key)?
        .as_str()
        .filter(|s| !s.is_empty())
}

/// Evaluates whether reflection is healthy enough to discard unreflected journals. Returns `None`
/// if `admit` returns `false` mid-walk so partial reads never open or close the gate.
pub fn read_unreflected_retention_gate(
    reflection_dir: &Path,
    reflection_enabled: bool,
    admit: &mut dyn FnMut() -> bool,
) -> Option<UnreflectedRetentionGate> {
    if !reflection_enabled {
        return Some(UnreflectedRetentionGate {
            open: true,
            newest: None,
        });
    }
    let park = read_json(&reflection_dir.join("park.json"));
    if let Some(parked_at) = string_field(park.as_ref(), "parkedAt") {
        return Some(UnreflectedRetentionGate {
            open: false,
            newest: Some(GateNewestFailure {
                kind: ReflectionFailureKind::Parked,
                at: parked_at.to_owned(),
            }),
        });
    }

    let mut last_success: Option<i64> = None;
    let mut last_failure: Option<(ReflectionFailureKind, i64)> = None;
    let mut record_failure = |kind: ReflectionFailureKind, at: i64| {
        if last_failure.is_none_or(|(_, prev)| at > prev) {
            last_failure = Some((kind, at));
        }
    };

    let completions = reflection_dir.join("completions");
    for name in list_names(&completions) {
        if !name.ends_with(".json") {
            continue;
        }
        if !admit() {
            return None;
        }
        let rec = read_json(&completions.join(name));
        let Some(outcome) = string_field(rec.as_ref(), "outcome") else {
            continue;
        };
        let Some(at) = string_field(rec.as_ref(), "finishedAt").and_then(parse_rfc3339) else {
            continue;
        };
        if outcome == "merged" || outcome == "no_changes" {
            last_success = Some(last_success.map_or(at, |prev| prev.max(at)));
        } else {
            record_failure(ReflectionFailureKind::Failed, at);
        }
    }

    let runs = reflection_dir.join("runs");
    for name in list_names(&runs) {
        if !admit() {
            return None;
        }
        let run_dir = runs.join(&name);
        let abandoned = read_json(&run_dir.join("abandoned.json"));
        if let Some(at) = string_field(abandoned.as_ref(), "generation").and_then(parse_rfc3339) {
            record_failure(ReflectionFailureKind::Abandoned, at);
        }
        let quarantined = read_json(&run_dir.join("quarantined.json"));
        if let Some(at) = string_field(quarantined.as_ref(), "generation").and_then(parse_rfc3339) {
            record_failure(ReflectionFailureKind::Quarantined, at);
        }
    }

    match (last_failure, last_success) {
        (None, _) => Some(UnreflectedRetentionGate {
            open: true,
            newest: None,
        }),
        (Some((_, fail_at)), Some(succ_at)) if succ_at >= fail_at => {
            Some(UnreflectedRetentionGate {
                open: true,
                newest: None,
            })
        }
        (Some((kind, fail_at)), _) => Some(UnreflectedRetentionGate {
            open: false,
            newest: Some(GateNewestFailure {
                kind,
                at: format_rfc3339_millis(fail_at),
            }),
        }),
    }
}
