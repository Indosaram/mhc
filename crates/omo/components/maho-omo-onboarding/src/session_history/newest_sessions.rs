//! Digests of the newest recent sessions (upstream `newest-sessions.ts`).

use std::{
    io,
    path::Path,
    sync::{Arc, atomic::Ordering},
};

use super::agent_transcripts::unique_homes;
use super::constants::{MAX_DISCOVERY_OPERATIONS_PER_SOURCE, MAX_IMPORT_RECORDS};
use super::project_paths::ProjectPathRules;
use super::session_digest::build_session_digest;
use super::session_history_fs::{
    AbortFlag, FileKind, FileStats, SessionHistoryFile, SessionHistoryFs, real_path_or,
    stat_or_null,
};
use super::transcript_discovery::{
    TranscriptCandidate, discover_claude_transcripts, discover_codex_transcripts,
};
use super::transcript_parser::{TranscriptMetadata, parse_agent_session_records};
use super::transcript_reader::{read_transcript_records, transcript_identity};
use super::transcript_records::extract_decoded_cwd;
use super::types::{AgentHome, AgentSessionSource, SessionDigest};

fn aborted_error() -> io::Error {
    io::Error::new(io::ErrorKind::Interrupted, "aborted")
}

struct AbortableFile {
    inner: Box<dyn SessionHistoryFile>,
    signal: AbortFlag,
}

impl SessionHistoryFile for AbortableFile {
    fn read(&mut self, size: usize) -> io::Result<Option<Vec<u8>>> {
        if self.signal.load(Ordering::SeqCst) {
            return Err(aborted_error());
        }
        self.inner.read(size)
    }
    fn stat(&mut self) -> io::Result<FileStats> {
        if self.signal.load(Ordering::SeqCst) {
            return Err(aborted_error());
        }
        self.inner.stat()
    }
    /// Closing a handle always goes through.
    fn close(&mut self) -> io::Result<()> {
        self.inner.close()
    }
}

struct AbortableFs {
    inner: Arc<dyn SessionHistoryFs>,
    signal: AbortFlag,
}

impl AbortableFs {
    fn check(&self) -> io::Result<()> {
        if self.signal.load(Ordering::SeqCst) {
            Err(aborted_error())
        } else {
            Ok(())
        }
    }
}

impl SessionHistoryFs for AbortableFs {
    fn read_directory(&self, path: &str) -> io::Result<Vec<String>> {
        self.check()?;
        self.inner.read_directory(path)
    }
    fn stat(&self, path: &str) -> io::Result<FileStats> {
        self.check()?;
        self.inner.stat(path)
    }
    fn real_path(&self, path: &str) -> io::Result<String> {
        self.check()?;
        self.inner.real_path(path)
    }
    fn read_file_string(&self, path: &str) -> io::Result<String> {
        self.check()?;
        self.inner.read_file_string(path)
    }
    fn open(&self, path: &str) -> io::Result<Box<dyn SessionHistoryFile>> {
        self.check()?;
        let mut handle = self.inner.open(path)?;
        if self.signal.load(Ordering::SeqCst) {
            // A handle that opens only after the abort is closed rather than leaked.
            let _ = handle.close();
            return Err(aborted_error());
        }
        Ok(Box::new(AbortableFile {
            inner: handle,
            signal: Arc::clone(&self.signal),
        }))
    }
}

/// The filesystem as seen by a scan that must stop at `signal`: after the abort no call reaches `fs`.
/// Closing a handle always goes through, and a handle that opens only after the abort is closed.
pub fn abortable_session_history_fs(
    fs: Arc<dyn SessionHistoryFs>,
    signal: AbortFlag,
) -> Arc<dyn SessionHistoryFs> {
    Arc::new(AbortableFs { inner: fs, signal })
}

struct SourcedCandidate {
    transcript: TranscriptCandidate,
    source: AgentSessionSource,
}

fn candidates(
    fs: &dyn SessionHistoryFs,
    rules: &ProjectPathRules,
    homes: &[AgentHome],
    signal: &AbortFlag,
) -> Vec<SourcedCandidate> {
    let mut found = Vec::new();
    for source in AgentSessionSource::ALL {
        let source_homes = unique_homes(rules, homes, source);
        for home_path in &source_homes {
            if signal.load(Ordering::SeqCst) {
                return found;
            }
            // Every home gets the floor share, as upstream does (no remainder here).
            let budget = MAX_DISCOVERY_OPERATIONS_PER_SOURCE
                / i64::try_from(source_homes.len().max(1)).unwrap_or(1);
            let discovered = if source == AgentSessionSource::ClaudeAgent {
                discover_claude_transcripts(fs, home_path, budget, Some(signal.as_ref()))
            } else {
                discover_codex_transcripts(fs, home_path, budget, Some(signal.as_ref()))
            };
            found.extend(
                discovered
                    .transcripts
                    .into_iter()
                    .map(|transcript| SourcedCandidate { transcript, source }),
            );
        }
    }
    found
}

pub struct NewestSessionDigestsInput {
    pub fs: Arc<dyn SessionHistoryFs>,
    pub create_rules: Box<dyn Fn(Arc<dyn SessionHistoryFs>) -> Arc<ProjectPathRules>>,
    pub homes: Vec<AgentHome>,
    pub now_ms: i64,
    pub recent_window_ms: i64,
    pub limit: usize,
    pub signal: AbortFlag,
}

/// Digests of the newest `limit` recent agent sessions across Claude Code and Codex. Discovery only lists
/// directories and stats files to order them; at most `limit` transcripts are then opened, newest first.
/// A transcript that turns out unusable (excluded project, unparsable, a duplicate session) still spends
/// one of those reads. Once `signal` is set nothing more is read and the digests made so far are returned.
pub fn newest_session_digests(input: NewestSessionDigestsInput) -> Vec<SessionDigest> {
    let NewestSessionDigestsInput {
        fs,
        create_rules,
        homes,
        now_ms,
        recent_window_ms,
        limit,
        signal,
    } = input;
    let fs = abortable_session_history_fs(fs, Arc::clone(&signal));
    let rules = create_rules(Arc::clone(&fs));
    let cutoff_ms = now_ms - recent_window_ms;
    let mut recent: Vec<SourcedCandidate> =
        candidates(fs.as_ref(), rules.as_ref(), &homes, &signal)
            .into_iter()
            .filter(|c| c.transcript.mtime_ms >= cutoff_ms && c.transcript.mtime_ms <= now_ms)
            .collect();
    recent.sort_by(|left, right| {
        right
            .transcript
            .mtime_ms
            .cmp(&left.transcript.mtime_ms)
            .then_with(|| left.transcript.file_path.cmp(&right.transcript.file_path))
    });
    let mut digests = Vec::new();
    let mut sessions = std::collections::HashSet::new();
    let mut reads = 0_usize;
    let mut records_remaining = MAX_IMPORT_RECORDS;
    for candidate in recent {
        if signal.load(Ordering::SeqCst)
            || reads == limit
            || digests.len() == limit
            || records_remaining == 0
        {
            break;
        }
        let transcript = &candidate.transcript;
        let Some(stats) =
            stat_or_null(fs.as_ref(), &transcript.file_path).filter(|s| s.kind == FileKind::File)
        else {
            continue;
        };
        reads += 1;
        let snapshot = read_transcript_records(
            fs.as_ref(),
            &transcript_identity(&transcript.file_path, &stats),
            records_remaining,
            candidate.source,
        );
        if signal.load(Ordering::SeqCst) {
            continue;
        }
        let Some(snapshot) = snapshot else { continue };
        records_remaining = records_remaining.saturating_sub(snapshot.record_count);
        let recorded = snapshot.records.iter().find_map(extract_decoded_cwd);
        let Some(cwd) = recorded
            .as_deref()
            .and_then(|cwd| rules.resolve_recorded_path(cwd))
        else {
            continue;
        };
        if rules.is_excluded_project_path(&cwd)
            || rules.is_excluded_project_path(&real_path_or(fs.as_ref(), &cwd))
        {
            continue;
        }
        let fallback_session_id = Path::new(&transcript.file_path)
            .file_name()
            .map(|n| n.to_string_lossy().trim_end_matches(".jsonl").to_owned())
            .unwrap_or_default();
        let metadata = TranscriptMetadata {
            source: candidate.source,
            fallback_session_id,
            last_active_at_ms: transcript.mtime_ms,
        };
        let Some(thread) = parse_agent_session_records(&metadata, &snapshot.records) else {
            continue;
        };
        if !sessions.insert(format!(
            "{}\0{}",
            thread.source.as_str(),
            thread.provider_session_id
        )) {
            continue;
        }
        digests.push(build_session_digest(&thread, &snapshot.records, &cwd));
    }
    digests
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_history::project_paths::{ProjectPathRulesInput, create_project_path_rules};
    use crate::session_history::session_history_fs::node_session_history_fs;
    use std::sync::atomic::AtomicBool;

    fn write_session(
        home: &Path,
        project: &str,
        name: &str,
        cwd: &str,
        session: &str,
    ) -> Result<(), std::io::Error> {
        let dir = home.join("projects").join(project);
        std::fs::create_dir_all(&dir)?;
        let body = format!(
            "{}\n",
            serde_json::json!({"type":"user","cwd":cwd,"sessionId":session,"message":{"role":"user","content":"hello"}})
        );
        std::fs::write(dir.join(name), body)
    }

    fn input(home: &Path, limit: usize, signal: AbortFlag) -> NewestSessionDigestsInput {
        let now_ms = jiff::Timestamp::now().as_millisecond() + 60_000;
        NewestSessionDigestsInput {
            fs: node_session_history_fs(),
            create_rules: Box::new(|fs| {
                Arc::new(create_project_path_rules(ProjectPathRulesInput {
                    home_dir: Some("/nonexistent-home".into()),
                    tmp_dir: Some("/nonexistent-tmp".into()),
                    excluded_directories: Some(vec![]),
                    managed_worktree_directories: None,
                    platform: Some("linux".into()),
                    fs,
                }))
            }),
            homes: vec![AgentHome {
                source: AgentSessionSource::ClaudeAgent,
                home_path: home.to_string_lossy().into_owned(),
            }],
            now_ms,
            recent_window_ms: 3_600_000,
            limit,
            signal,
        }
    }

    #[test]
    fn digests_are_limited_deduplicated_and_abortable() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let home = dir.path().join("claude");
        let project = dir.path().join("proj");
        std::fs::create_dir_all(&project)?;
        let cwd = project.to_string_lossy().into_owned();
        write_session(&home, "p", "a.jsonl", &cwd, "one")?;
        write_session(&home, "p", "b.jsonl", &cwd, "one")?;
        write_session(&home, "p", "c.jsonl", &cwd, "two")?;
        let all = newest_session_digests(input(&home, 10, Arc::new(AtomicBool::new(false))));
        assert_eq!(all.len(), 2);
        let limited = newest_session_digests(input(&home, 1, Arc::new(AtomicBool::new(false))));
        assert_eq!(limited.len(), 1);
        let aborted = newest_session_digests(input(&home, 10, Arc::new(AtomicBool::new(true))));
        assert!(aborted.is_empty());
        Ok(())
    }

    #[test]
    fn abortable_fs_rejects_after_abort_but_close_passes() -> Result<(), Box<dyn std::error::Error>>
    {
        let dir = tempfile::tempdir()?;
        let file = dir.path().join("x.jsonl");
        std::fs::write(&file, "{}")?;
        let signal: AbortFlag = Arc::new(AtomicBool::new(false));
        let fs = abortable_session_history_fs(node_session_history_fs(), Arc::clone(&signal));
        let path = file.to_string_lossy().into_owned();
        let mut handle = fs.open(&path)?;
        signal.store(true, Ordering::SeqCst);
        assert!(handle.read(1).is_err());
        assert!(handle.close().is_ok());
        assert!(fs.stat(&path).is_err());
        assert!(fs.open(&path).is_err());
        Ok(())
    }
}
