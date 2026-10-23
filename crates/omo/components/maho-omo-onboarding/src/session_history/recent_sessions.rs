//! Newest-first recent sessions of one project (upstream `recent-sessions.ts`).
//!
//! Each transcript is re-stat'ed and re-read, and its own cwd rechecked, because a file can be replaced after
//! discovery. Reads share the per-call caps of 100 transcripts, 4 GiB and 100,000 records. A transcript still
//! identical to one the caller names as completed is only stat'ed, never read, and does not count against them.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::Path,
    sync::Arc,
};

use super::constants::{
    DEFAULT_RECENT_WINDOW_MS, MAX_IMPORT_BYTES, MAX_IMPORT_RECORDS, MAX_IMPORT_TRANSCRIPTS,
    MAX_IMPORTED_TRANSCRIPT_BYTES,
};
use super::project_paths::ProjectPathRules;
use super::session_digest::build_session_digest;
use super::session_history_fs::{FileKind, SessionHistoryFs, real_path_or, stat_or_null};
use super::transcript_cwd::CwdGroup;
use super::transcript_parser::{TranscriptMetadata, parse_agent_session_records};
use super::transcript_reader::{
    read_transcript_records, same_transcript_identity, transcript_identity,
};
use super::transcript_records::extract_decoded_cwd;
use super::types::{
    AgentSessionSource, CompletedTranscript, RecentSessionOutcome, RecentSessionsQuery, ScanCounts,
    TranscriptSource,
};

#[derive(Clone, Debug)]
pub struct SourcedGroup {
    pub group: CwdGroup,
    pub source: AgentSessionSource,
}

struct Eligible {
    group: SourcedGroup,
    file_path: String,
    mtime_ms: i64,
    target_identity: String,
}

fn eligible_transcripts(
    fs: &dyn SessionHistoryFs,
    rules: &ProjectPathRules,
    groups: &[SourcedGroup],
    query: &RecentSessionsQuery,
    now_ms: i64,
) -> Vec<Eligible> {
    let root = query
        .workspace_root
        .as_deref()
        .and_then(|root| rules.resolve_recorded_path(root));
    if query.workspace_root.is_some() {
        match &root {
            None => return Vec::new(),
            Some(root) => {
                if rules.is_excluded_project_path(root)
                    || rules.is_excluded_project_path(&real_path_or(fs, root))
                {
                    return Vec::new();
                }
            }
        }
    }
    let root_identity = root
        .as_deref()
        .map(|root| rules.directory_identity(root, None));
    let cutoff_ms = now_ms - query.recent_window_ms.unwrap_or(DEFAULT_RECENT_WINDOW_MS);
    let mut eligible = Vec::new();
    for sourced in groups {
        let Some(resolved) = rules.resolve_recorded_path(&sourced.group.cwd) else {
            continue;
        };
        if root_identity.is_none()
            && (rules.is_excluded_project_path(&resolved)
                || rules.is_excluded_project_path(&real_path_or(fs, &resolved)))
        {
            continue;
        }
        let identity = rules.directory_identity(&resolved, None);
        if root_identity
            .as_ref()
            .is_some_and(|expected| *expected != identity)
        {
            continue;
        }
        for transcript in &sourced.group.transcripts {
            if transcript.mtime_ms < cutoff_ms || transcript.mtime_ms > now_ms {
                continue;
            }
            eligible.push(Eligible {
                group: sourced.clone(),
                file_path: transcript.file_path.clone(),
                mtime_ms: transcript.mtime_ms,
                target_identity: identity.clone(),
            });
        }
    }
    eligible.sort_by(|left, right| {
        right
            .mtime_ms
            .cmp(&left.mtime_ms)
            .then_with(|| left.file_path.cmp(&right.file_path))
    });
    eligible
}

pub type GroupsLoader = Box<dyn FnMut() -> Vec<SourcedGroup> + Send>;

pub struct RecentSessionsInput {
    pub fs: Arc<dyn SessionHistoryFs>,
    pub rules: Arc<ProjectPathRules>,
    pub groups: GroupsLoader,
    pub query: RecentSessionsQuery,
    pub now: Arc<dyn Fn() -> i64 + Send + Sync>,
}

/// An iterator of outcomes. `counts()` is final once iteration ends.
pub struct RecentSessionsRun {
    fs: Arc<dyn SessionHistoryFs>,
    rules: Arc<ProjectPathRules>,
    groups: Option<GroupsLoader>,
    query: RecentSessionsQuery,
    now: Arc<dyn Fn() -> i64 + Send + Sync>,
    completed_by_file: HashMap<String, Vec<CompletedTranscript>>,
    pending: Option<VecDeque<Eligible>>,
    counts: ScanCounts,
    imported_sessions: HashSet<String>,
    bytes_remaining: u64,
    transcripts_remaining: usize,
    records_remaining: usize,
}

/// Eligible transcripts found, transcripts read, and the per-call read cap.
impl RecentSessionsRun {
    pub fn counts(&self) -> ScanCounts {
        self.counts
    }

    fn completed_source(&self, entry: &Eligible) -> Option<TranscriptSource> {
        let key = format!("{}\0{}", entry.group.source.as_str(), entry.file_path);
        let named = self.completed_by_file.get(&key)?;
        let stats = stat_or_null(self.fs.as_ref(), &entry.file_path)?;
        if stats.kind != FileKind::File {
            return None;
        }
        let identity = transcript_identity(&entry.file_path, &stats);
        let matched = named
            .iter()
            .find(|completed| same_transcript_identity(&completed.identity(), &identity))?;
        Some(TranscriptSource {
            provider: entry.group.source,
            home_path: entry.group.group.home_path.clone(),
            provider_session_id: matched.provider_session_id.clone(),
            file_path: identity.file_path,
            size: identity.size,
            mtime_ms: identity.mtime_ms,
            device: identity.device,
            inode: identity.inode,
            birthtime_ms: identity.birthtime_ms,
        })
    }

    fn process(&mut self, entry: &Eligible) -> RecentSessionOutcome {
        if let Some(imported) = self.completed_source(entry) {
            self.imported_sessions.insert(format!(
                "{}\0{}",
                imported.provider.as_str(),
                imported.provider_session_id
            ));
            return RecentSessionOutcome::AlreadyImported { source: imported };
        }
        if self.transcripts_remaining == 0
            || self.bytes_remaining == 0
            || self.records_remaining == 0
        {
            return RecentSessionOutcome::Skipped;
        }
        let fs = Arc::clone(&self.fs);
        let rules = Arc::clone(&self.rules);
        let Some(stats) =
            stat_or_null(fs.as_ref(), &entry.file_path).filter(|s| s.kind == FileKind::File)
        else {
            return RecentSessionOutcome::Skipped;
        };
        let identity = transcript_identity(&entry.file_path, &stats);
        if identity.size > MAX_IMPORTED_TRANSCRIPT_BYTES || identity.size > self.bytes_remaining {
            return RecentSessionOutcome::Skipped;
        }
        self.transcripts_remaining -= 1;
        self.bytes_remaining -= identity.size;
        self.counts.read += 1;
        let Some(snapshot) = read_transcript_records(
            fs.as_ref(),
            &identity,
            self.records_remaining,
            entry.group.source,
        ) else {
            return RecentSessionOutcome::Skipped;
        };
        self.records_remaining = self.records_remaining.saturating_sub(snapshot.record_count);
        let snapshot_cwd = snapshot.records.iter().find_map(extract_decoded_cwd);
        let resolved_cwd = snapshot_cwd
            .as_deref()
            .and_then(|cwd| rules.resolve_recorded_path(cwd));
        let Some(resolved_cwd) =
            resolved_cwd.filter(|cwd| rules.directory_identity(cwd, None) == entry.target_identity)
        else {
            return RecentSessionOutcome::Skipped;
        };
        let fallback_session_id = Path::new(&entry.file_path)
            .file_name()
            .map(|n| n.to_string_lossy().trim_end_matches(".jsonl").to_owned())
            .unwrap_or_default();
        let metadata = TranscriptMetadata {
            source: entry.group.source,
            fallback_session_id,
            last_active_at_ms: entry.mtime_ms,
        };
        let Some(thread) = parse_agent_session_records(&metadata, &snapshot.records) else {
            return RecentSessionOutcome::Skipped;
        };
        let source = TranscriptSource {
            provider: thread.source,
            home_path: entry.group.group.home_path.clone(),
            provider_session_id: thread.provider_session_id.clone(),
            file_path: identity.file_path,
            size: identity.size,
            mtime_ms: identity.mtime_ms,
            device: identity.device,
            inode: identity.inode,
            birthtime_ms: identity.birthtime_ms,
        };
        let session_key = format!("{}\0{}", thread.source.as_str(), thread.provider_session_id);
        if !self.imported_sessions.insert(session_key) {
            return RecentSessionOutcome::Duplicate { source };
        }
        let digest = build_session_digest(&thread, &snapshot.records, &resolved_cwd);
        RecentSessionOutcome::Importable {
            thread,
            source,
            cwd: resolved_cwd,
            digest: Box::new(digest),
        }
    }
}

impl Iterator for RecentSessionsRun {
    type Item = RecentSessionOutcome;

    fn next(&mut self) -> Option<Self::Item> {
        if self.pending.is_none() {
            let groups = self
                .groups
                .take()
                .map(|mut load| load())
                .unwrap_or_default();
            let eligible = eligible_transcripts(
                self.fs.as_ref(),
                self.rules.as_ref(),
                &groups,
                &self.query,
                (self.now)(),
            );
            self.counts.found = eligible.len();
            self.pending = Some(eligible.into());
        }
        let entry = self.pending.as_mut()?.pop_front()?;
        Some(self.process(&entry))
    }
}

pub fn list_recent_sessions(input: RecentSessionsInput) -> RecentSessionsRun {
    let mut completed_by_file: HashMap<String, Vec<CompletedTranscript>> = HashMap::new();
    for completed in input.query.completed.iter().flatten() {
        completed_by_file
            .entry(format!(
                "{}\0{}",
                completed.provider.as_str(),
                completed.file_path
            ))
            .or_default()
            .push(completed.clone());
    }
    RecentSessionsRun {
        fs: input.fs,
        rules: input.rules,
        groups: Some(input.groups),
        query: input.query,
        now: input.now,
        completed_by_file,
        pending: None,
        counts: ScanCounts {
            found: 0,
            read: 0,
            cap: MAX_IMPORT_TRANSCRIPTS,
        },
        imported_sessions: HashSet::new(),
        bytes_remaining: MAX_IMPORT_BYTES,
        transcripts_remaining: MAX_IMPORT_TRANSCRIPTS,
        records_remaining: MAX_IMPORT_RECORDS,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_history::project_paths::{ProjectPathRulesInput, create_project_path_rules};
    use crate::session_history::session_history_fs::{
        NodeSessionHistoryFs, node_session_history_fs,
    };
    use crate::session_history::transcript_cwd::CwdTranscript;

    struct Fixture {
        _dir: tempfile::TempDir,
        project: String,
        files: Vec<String>,
    }

    fn fixture(bodies: &[(&str, String)]) -> Result<Fixture, Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let project = dir.path().join("proj");
        std::fs::create_dir_all(&project)?;
        let mut files = Vec::new();
        for (name, body) in bodies {
            let path = dir.path().join(name);
            std::fs::write(&path, body)?;
            files.push(path.to_string_lossy().into_owned());
        }
        Ok(Fixture {
            project: project.to_string_lossy().into_owned(),
            _dir: dir,
            files,
        })
    }

    fn claude_body(cwd: &str, session: &str) -> String {
        format!(
            "{}\n{}\n",
            serde_json::json!({"type":"user","cwd":cwd,"sessionId":session,"message":{"role":"user","content":"do it"}}),
            serde_json::json!({"type":"assistant","message":{"role":"assistant","content":"done"}}),
        )
    }

    fn run(
        fx: &Fixture,
        query: RecentSessionsQuery,
        mtime: i64,
    ) -> Result<RecentSessionsRun, Box<dyn std::error::Error>> {
        let fs = node_session_history_fs();
        let rules = Arc::new(create_project_path_rules(ProjectPathRulesInput {
            home_dir: Some("/nonexistent-home".into()),
            tmp_dir: Some("/nonexistent-tmp".into()),
            excluded_directories: Some(vec![]),
            managed_worktree_directories: None,
            platform: Some("linux".into()),
            fs: fs.clone(),
        }));
        let transcripts: Vec<CwdTranscript> = fx
            .files
            .iter()
            .enumerate()
            .map(|(i, f)| CwdTranscript {
                file_path: f.clone(),
                mtime_ms: mtime - i64::try_from(i).unwrap_or(0),
            })
            .collect();
        let group = SourcedGroup {
            group: CwdGroup {
                cwd: fx.project.clone(),
                home_path: "h".into(),
                last_active_at_ms: mtime,
                transcripts,
            },
            source: AgentSessionSource::ClaudeAgent,
        };
        Ok(list_recent_sessions(RecentSessionsInput {
            fs,
            rules,
            groups: Box::new(move || vec![group.clone()]),
            query,
            now: Arc::new(move || mtime + 1000),
        }))
    }

    #[test]
    fn yields_importable_then_duplicate_and_counts_reads() -> Result<(), Box<dyn std::error::Error>>
    {
        let fx = fixture(&[("a.jsonl", String::new()), ("b.jsonl", String::new())])?;
        std::fs::write(&fx.files[0], claude_body(&fx.project, "same"))?;
        std::fs::write(&fx.files[1], claude_body(&fx.project, "same"))?;
        let query = RecentSessionsQuery {
            workspace_root: Some(fx.project.clone()),
            ..Default::default()
        };
        let mut run = run(&fx, query, 1_700_000_000_000)?;
        let tags: Vec<&str> = run.by_ref().map(|o| o.tag()).collect::<Vec<_>>();
        assert_eq!(tags, vec!["Importable", "Duplicate"]);
        assert_eq!(
            run.counts(),
            ScanCounts {
                found: 2,
                read: 2,
                cap: 100
            }
        );
        Ok(())
    }

    #[test]
    fn mismatched_cwd_is_skipped_and_completed_is_not_read()
    -> Result<(), Box<dyn std::error::Error>> {
        let fx = fixture(&[("a.jsonl", String::new()), ("b.jsonl", String::new())])?;
        std::fs::write(&fx.files[0], claude_body("/somewhere/else", "x"))?;
        std::fs::write(&fx.files[1], claude_body(&fx.project, "done-before"))?;
        let stats = crate::session_history::session_history_fs::stat_or_null(
            &NodeSessionHistoryFs,
            &fx.files[1],
        )
        .ok_or("stat")?;
        let completed = CompletedTranscript {
            provider: AgentSessionSource::ClaudeAgent,
            provider_session_id: "done-before".into(),
            file_path: fx.files[1].clone(),
            size: stats.size,
            mtime_ms: stats.mtime_ms,
            device: stats.dev,
            inode: stats.ino,
            birthtime_ms: stats.birthtime_ms,
        };
        let query = RecentSessionsQuery {
            completed: Some(vec![completed]),
            ..Default::default()
        };
        let mut run = run(&fx, query, 1_700_000_000_000)?;
        let tags: Vec<&str> = run.by_ref().map(|o| o.tag()).collect::<Vec<_>>();
        assert_eq!(tags, vec!["Skipped", "AlreadyImported"]);
        assert_eq!(run.counts().read, 1);
        Ok(())
    }

    #[test]
    fn outside_the_window_is_not_eligible() -> Result<(), Box<dyn std::error::Error>> {
        let fx = fixture(&[("a.jsonl", String::new())])?;
        std::fs::write(&fx.files[0], claude_body(&fx.project, "s"))?;
        let query = RecentSessionsQuery {
            recent_window_ms: Some(1),
            ..Default::default()
        };
        let mut run = run(&fx, query, 1_700_000_000_000)?;
        // `now` is mtime + 1000 and the window is 1 ms, so nothing is recent.
        assert_eq!(run.by_ref().count(), 0);
        assert_eq!(run.counts().found, 0);
        Ok(())
    }
}
