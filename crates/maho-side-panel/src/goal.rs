//! The goal state file reader (spec 5 Goal). The file is re-parsed only when its mtime or size
//! changes; malformed files and unknown statuses hide the section.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Goal status values shown in the heading.
pub const STATUSES: [&str; 5] = [
    "active",
    "paused",
    "blocked",
    "limited by budget",
    "complete",
];

/// The parsed goal facts the panel shows.
#[derive(Clone, Debug, PartialEq)]
pub struct Goal {
    pub status: String,
    pub objective: String,
    pub tokens_used: u64,
    pub token_budget: Option<u64>,
    pub time_used_seconds: Option<f64>,
    pub continuations: u64,
}

impl Goal {
    /// Budget use in percent when a token budget is set.
    pub fn budget_percent(&self) -> Option<f64> {
        self.token_budget
            .filter(|b| *b > 0)
            .map(|b| self.tokens_used as f64 * 100.0 / b as f64)
    }
}

fn normalize_status(raw: &str) -> Option<String> {
    let lowered = raw.trim().to_ascii_lowercase().replace(['_', '-'], " ");
    let status = match lowered.as_str() {
        "budget limited" | "budgetlimited" | "limited" => "limited by budget",
        other => other,
    };
    STATUSES.contains(&status).then(|| status.to_owned())
}

/// Parses goal file text (a JSON object, or JSON lines whose last object wins).
pub fn parse(text: &str) -> Option<Goal> {
    let value: Value = serde_json::from_str(text.trim()).ok().or_else(|| {
        text.lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .and_then(|l| serde_json::from_str(l).ok())
    })?;
    let value = value
        .get("goal")
        .filter(|g| g.is_object())
        .unwrap_or(&value);
    let status = normalize_status(value.get("status")?.as_str()?)?;
    let objective = value.get("objective")?.as_str()?.to_owned();
    let number = |key: &str| value.get(key).and_then(Value::as_u64);
    Some(Goal {
        status,
        objective,
        tokens_used: number("tokensUsed").unwrap_or(0),
        token_budget: number("tokenBudget"),
        time_used_seconds: value.get("timeUsedSeconds").and_then(Value::as_f64),
        continuations: number("consecutiveContinuations").unwrap_or(0),
    })
}

/// Cached reader keyed on (mtime, size).
#[derive(Debug, Default)]
pub struct GoalReader {
    key: Option<(PathBuf, Option<SystemTime>, u64)>,
    cached: Option<Goal>,
    /// Number of parses performed (observable for tests).
    pub parses: u64,
}

impl GoalReader {
    /// The goal of `path`; re-parses only when mtime or size changed.
    pub fn read(&mut self, path: &Path) -> Option<Goal> {
        let Ok(meta) = std::fs::metadata(path) else {
            self.key = None;
            self.cached = None;
            return None;
        };
        let key = (path.to_path_buf(), meta.modified().ok(), meta.len());
        if self.key.as_ref() == Some(&key) {
            return self.cached.clone();
        }
        self.parses += 1;
        self.cached = std::fs::read_to_string(path).ok().and_then(|t| parse(&t));
        self.key = Some(key);
        self.cached.clone()
    }
}
