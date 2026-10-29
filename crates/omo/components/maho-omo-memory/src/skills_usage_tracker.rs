use std::{collections::BTreeMap, path::{Component, Path, PathBuf}};
use crate::skills_usage_ledger::{SkillsUsageLedgerPath, create_skills_usage_lock_record, increment_skills_usage_batch};

pub const DEBOUNCE_MS: u64 = 500;

pub fn extract_skill_id(repo_dir: &Path, raw_path: &str, cwd: &Path) -> Option<String> {
    if raw_path.is_empty() || raw_path.contains('\0') { return None; }
    let path = Path::new(raw_path);
    let absolute = if path.is_absolute() { path.to_path_buf() } else { cwd.join(path) };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {},
            Component::ParentDir => { normalized.pop(); },
            other => normalized.push(other.as_os_str()),
        }
    }
    let relative = normalized.strip_prefix(repo_dir).ok()?;
    let mut segments = relative.components();
    if segments.next()?.as_os_str() != "skills" { return None; }
    segments.next()?.as_os_str().to_str().map(str::to_owned)
}

pub struct SkillsUsageTracker {
    paths: SkillsUsageLedgerPath,
    repo_dir: PathBuf,
    pending: BTreeMap<String, f64>,
    flush_at_ms: Option<u64>,
}

impl SkillsUsageTracker {
    pub fn new(paths: SkillsUsageLedgerPath, repo_dir: PathBuf) -> Self {
        Self { paths, repo_dir, pending: BTreeMap::new(), flush_at_ms: None }
    }

    pub fn record_read(&mut self, raw_path: &str, cwd: &Path, now_ms: u64) -> Option<u64> {
        let skill_id = extract_skill_id(&self.repo_dir, raw_path, cwd)?;
        *self.pending.entry(skill_id).or_default() += 1.0;
        Some(*self.flush_at_ms.get_or_insert(now_ms.saturating_add(DEBOUNCE_MS)))
    }

    pub fn flush(&mut self, now: impl FnOnce() -> String, aborted: Option<&dyn Fn() -> bool>, warn: impl FnOnce(&str)) {
        if aborted.is_some_and(|probe| probe()) { return; }
        self.flush_at_ms = None;
        if self.pending.is_empty() { return; }
        let batch = std::mem::take(&mut self.pending);
        let record = match create_skills_usage_lock_record() {
            Ok(record) => record,
            Err(error) => { warn(&error.to_string()); return; }
        };
        if aborted.is_some_and(|probe| probe()) { return; }
        if let Err(error) = increment_skills_usage_batch(&self.paths, &batch, now, &record, aborted) { warn(&error.to_string()); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skills_usage_ledger::{read_skills_usage_ledger, skills_usage_paths};

    #[test]
    fn single_read_persists_under_ledger_lock() {
        let root = tempfile::tempdir().unwrap();
        let paths = skills_usage_paths(&root.path().join("runtime"), &root.path().join("locks"));
        let ledger = paths.ledger_path.clone();
        let mut tracker = SkillsUsageTracker::new(paths, root.path().into());
        assert_eq!(tracker.record_read("skills/foo/SKILL.md", root.path(), 10), Some(510));
        tracker.flush(|| "2026-01-15T10:00:00.000Z".into(), None, |e| panic!("{e}"));
        let entry = &read_skills_usage_ledger(&ledger)["foo"];
        assert_eq!(entry.count, 1.0);
        assert_eq!(entry.last_used_at, "2026-01-15T10:00:00.000Z");
    }

    #[test]
    fn non_skill_read_does_not_create_ledger() {
        let root = tempfile::tempdir().unwrap();
        let paths = skills_usage_paths(root.path(), &root.path().join("locks"));
        let ledger = paths.ledger_path.clone();
        let mut tracker = SkillsUsageTracker::new(paths, root.path().into());
        assert_eq!(tracker.record_read("notes/facts.md", root.path(), 0), None);
        tracker.flush(|| "now".into(), None, |e| panic!("{e}"));
        assert!(!ledger.exists());
    }

    #[test]
    fn repeated_reads_share_first_deadline_and_accumulate() {
        let root = tempfile::tempdir().unwrap();
        let paths = skills_usage_paths(root.path(), &root.path().join("locks"));
        let ledger = paths.ledger_path.clone();
        let mut tracker = SkillsUsageTracker::new(paths, root.path().into());
        for now in [0, 100, 200] { assert_eq!(tracker.record_read("skills/foo/SKILL.md", root.path(), now), Some(500)); }
        tracker.flush(|| "now".into(), None, |e| panic!("{e}"));
        assert_eq!(read_skills_usage_ledger(&ledger)["foo"].count, 3.0);
    }

    #[test]
    fn multiple_skills_persist_together() {
        let root = tempfile::tempdir().unwrap();
        let paths = skills_usage_paths(root.path(), &root.path().join("locks"));
        let ledger = paths.ledger_path.clone();
        let mut tracker = SkillsUsageTracker::new(paths, root.path().into());
        for skill in ["foo", "bar", "foo"] { tracker.record_read(&format!("skills/{skill}/SKILL.md"), root.path(), 0); }
        tracker.flush(|| "now".into(), None, |e| panic!("{e}"));
        let ledger = read_skills_usage_ledger(&ledger);
        assert_eq!(ledger["foo"].count, 2.0);
        assert_eq!(ledger["bar"].count, 1.0);
    }

    #[test]
    fn cancelled_flush_retains_batch_and_path_escape_is_ignored() {
        let root = tempfile::tempdir().unwrap();
        let paths = skills_usage_paths(root.path(), &root.path().join("locks"));
        let ledger = paths.ledger_path.clone();
        let mut tracker = SkillsUsageTracker::new(paths, root.path().into());
        tracker.record_read("skills/foo/SKILL.md", root.path(), 0);
        for path in ["../skills/foreign/file", "skills/../../escape", "", "skills/\0bad"] { assert!(tracker.record_read(path, root.path(), 0).is_none()); }
        tracker.flush(|| "now".into(), Some(&|| true), |e| panic!("{e}"));
        assert!(!ledger.exists());
        tracker.flush(|| "now".into(), None, |e| panic!("{e}"));
        assert_eq!(read_skills_usage_ledger(&ledger)["foo"].count, 1.0);
    }
}
