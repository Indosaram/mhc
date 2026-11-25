//! Retention policy windows, keep reasons, and directory helpers (#12532, #12538, #10144).

use std::collections::BTreeMap;
use std::path::Path;
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};

use crate::identity::layout::MemoryIdentityPaths;

pub const DAY_MS: i64 = 24 * 60 * 60 * 1000;

/// Windows in days (`memory.runtime.*` in configuration, per-agent overridable).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeRetentionPolicy {
    /// Folded reflection runs, fully-reflected transcripts, idle sidecars, legacy stores.
    pub retention_days: u64,
    /// Never- and partly-reflected transcript journals, when `discard_unreflected_journals` is on.
    pub abandoned_journal_days: u64,
    /// Off (default) keeps every journal reflection never folded; on is reflection-health gated.
    pub discard_unreflected_journals: bool,
    /// Consumed completion records and recall ledgers.
    pub completions_days: u64,
    /// Wall-clock budget (ms) of one pass per identity; an interrupted pass resumes from cursor.
    pub pass_budget_ms: u64,
}

pub const DEFAULT_RUNTIME_RETENTION: RuntimeRetentionPolicy = RuntimeRetentionPolicy {
    retention_days: 7,
    abandoned_journal_days: 14,
    discard_unreflected_journals: false,
    completions_days: 30,
    pass_budget_ms: 60_000,
};

impl Default for RuntimeRetentionPolicy {
    fn default() -> Self {
        DEFAULT_RUNTIME_RETENTION
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RetentionKeepReason {
    Anchor,
    Locked,
    Live,
    Malformed,
    Newest,
    NoMark,
    NoTerminal,
    Owned,
    PendingCompaction,
    PendingDelivery,
    Pruned,
    Recent,
    Reserved,
    RunPresent,
    StateUnreadable,
    UnfoldedTerminal,
    Unclassified,
    SchedulerBusy,
    ClaimFailed,
    ReflectionUnhealthy,
    UnreflectedRetentionOff,
    ChangedDuringRemoval,
    StateBehindTranscript,
}

impl RetentionKeepReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Anchor => "anchor",
            Self::Locked => "locked",
            Self::Live => "live",
            Self::Malformed => "malformed",
            Self::Newest => "newest",
            Self::NoMark => "no-mark",
            Self::NoTerminal => "no-terminal",
            Self::Owned => "owned",
            Self::PendingCompaction => "pending-compaction",
            Self::PendingDelivery => "pending-delivery",
            Self::Pruned => "pruned",
            Self::Recent => "recent",
            Self::Reserved => "reserved",
            Self::RunPresent => "run-present",
            Self::StateUnreadable => "state-unreadable",
            Self::UnfoldedTerminal => "unfolded-terminal",
            Self::Unclassified => "unclassified",
            Self::SchedulerBusy => "scheduler-busy",
            Self::ClaimFailed => "claim-failed",
            Self::ReflectionUnhealthy => "reflection-unhealthy",
            Self::UnreflectedRetentionOff => "unreflected-retention-off",
            Self::ChangedDuringRemoval => "changed-during-removal",
            Self::StateBehindTranscript => "state-behind-transcript",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RetentionStoreResult {
    pub removed: Vec<String>,
    pub kept: BTreeMap<String, RetentionKeepReason>,
}

#[derive(Default)]
pub struct StoreTally {
    pub removed: Vec<String>,
    pub kept: BTreeMap<String, RetentionKeepReason>,
}

impl StoreTally {
    pub fn keep(&mut self, name: impl Into<String>, reason: RetentionKeepReason) {
        self.kept.insert(name.into(), reason);
    }

    pub fn result(self) -> RetentionStoreResult {
        RetentionStoreResult {
            removed: self.removed,
            kept: self.kept,
        }
    }
}

pub type WarnCallback<'a> = &'a dyn Fn(&str, &str);
pub type BeforeClaimRenameCallback<'a> = &'a dyn Fn(&Path, &Path) -> Result<(), String>;

pub struct RetentionContext<'a> {
    pub paths: &'a MemoryIdentityPaths,
    pub policy: RuntimeRetentionPolicy,
    pub now_ms: &'a dyn Fn() -> i64,
    pub is_live_session: Option<&'a dyn Fn(&str) -> bool>,
    /// `memory.reflection.enabled` for the identity; CLI & host sweep pass `true`.
    pub reflection_enabled: bool,
    pub aborted: Option<&'a dyn Fn() -> bool>,
    pub warn: Option<WarnCallback<'a>>,
    pub before_claim_rename: Option<BeforeClaimRenameCallback<'a>>,
    pub before_journal_claim: Option<&'a dyn Fn(&str)>,
    pub before_tombstone_delete: Option<&'a dyn Fn(&Path)>,
}

impl RetentionContext<'_> {
    pub fn now(&self) -> i64 {
        (self.now_ms)()
    }

    pub fn is_aborted(&self) -> bool {
        self.aborted.is_some_and(|f| f())
    }

    pub fn warn_msg(&self, msg: &str, detail: &str) {
        if let Some(warn) = self.warn {
            warn(msg, detail);
        }
    }
}

/// Sorted entry names in `dir`, or empty when `dir` is absent or not a directory.
pub fn list_names(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    names.sort();
    names
}

/// Newest mtime across `path` and its descendants (non-following symlinks), in epoch ms.
pub fn newest_mtime(path: &Path) -> Option<i64> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    let mtime = meta.modified().ok()?;
    let mut newest = i64::try_from(mtime.duration_since(UNIX_EPOCH).ok()?.as_millis()).ok()?;
    if !meta.is_dir() {
        return Some(newest);
    }
    for name in list_names(path) {
        if let Some(inner) = newest_mtime(&path.join(name)) {
            newest = newest.max(inner);
        }
    }
    Some(newest)
}

/// True when `path` exists and its newest mtime is at or before `cutoff_ms`.
pub fn idle_since(path: &Path, cutoff_ms: i64) -> bool {
    newest_mtime(path).is_some_and(|newest| newest <= cutoff_ms)
}

pub fn cutoff_for_days(now_ms: i64, days: u64) -> i64 {
    let span = i64::try_from(days)
        .unwrap_or(i64::MAX)
        .saturating_mul(DAY_MS);
    now_ms.saturating_sub(span)
}
