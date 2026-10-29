//! Transcript discovery under an agent home (upstream `transcript-discovery.ts`).

use std::{
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

use super::constants::MAX_TRANSCRIPTS_PER_SOURCE;
use super::session_history_fs::{
    FileKind, SessionHistoryFs, list_directory_or_empty, stat_or_null,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TranscriptCandidate {
    pub file_path: String,
    pub mtime_ms: i64,
    /// The agent home the transcript was found in; one home owns each copy of a session.
    pub home_path: String,
    pub size: u64,
}

#[derive(Clone, Debug, Default)]
pub struct DiscoveredTranscripts {
    pub transcripts: Vec<TranscriptCandidate>,
    pub truncated: bool,
}

pub fn join(parent: &str, child: &str) -> String {
    Path::new(parent).join(child).to_string_lossy().into_owned()
}

struct OperationBudget<'a> {
    fs: &'a dyn SessionHistoryFs,
    remaining: i64,
    signal: Option<&'a AtomicBool>,
    truncated: bool,
}

impl<'a> OperationBudget<'a> {
    fn aborted(&self) -> bool {
        self.signal
            .is_some_and(|signal| signal.load(Ordering::SeqCst))
    }

    fn exhausted(&mut self) -> bool {
        if self.remaining <= 0 || self.aborted() {
            self.truncated = true;
        }
        self.truncated
    }

    fn read_directory(&mut self, directory: &str) -> Vec<String> {
        if self.exhausted() {
            return Vec::new();
        }
        self.remaining -= 1;
        list_directory_or_empty(self.fs, directory)
    }

    fn stat_transcript(&mut self, file_path: &str, home_path: &str) -> Option<TranscriptCandidate> {
        if self.aborted() {
            return None;
        }
        self.remaining -= 1;
        let stats = stat_or_null(self.fs, file_path)?;
        if stats.kind != FileKind::File {
            return None;
        }
        Some(TranscriptCandidate {
            file_path: file_path.to_owned(),
            mtime_ms: stats.mtime_ms?,
            home_path: home_path.to_owned(),
            size: stats.size,
        })
    }
}

/// `<home>/projects/*/*.jsonl`
pub fn discover_claude_transcripts(
    fs: &dyn SessionHistoryFs,
    home_path: &str,
    operation_budget: i64,
    signal: Option<&AtomicBool>,
) -> DiscoveredTranscripts {
    let mut budget = OperationBudget {
        fs,
        remaining: operation_budget,
        signal,
        truncated: false,
    };
    let projects_dir = join(home_path, "projects");
    let mut transcripts = Vec::new();
    for project_directory in budget.read_directory(&projects_dir) {
        if budget.exhausted() {
            break;
        }
        let directory = join(&projects_dir, &project_directory);
        let entries: Vec<String> = budget
            .read_directory(&directory)
            .into_iter()
            .filter(|e| e.ends_with(".jsonl"))
            .collect();
        for entry in entries {
            if budget.exhausted() {
                break;
            }
            if let Some(transcript) = budget.stat_transcript(&join(&directory, &entry), home_path) {
                transcripts.push(transcript);
            }
        }
    }
    DiscoveredTranscripts {
        transcripts,
        truncated: budget.truncated,
    }
}

fn newest_first(mut entries: Vec<String>) -> Vec<String> {
    entries.sort();
    entries.reverse();
    entries
}

/// Date-partitioned directories sort chronologically, so walking them in reverse spends the budget on recent sessions.
pub fn discover_codex_transcripts(
    fs: &dyn SessionHistoryFs,
    home_path: &str,
    operation_budget: i64,
    signal: Option<&AtomicBool>,
) -> DiscoveredTranscripts {
    let mut budget = OperationBudget {
        fs,
        remaining: operation_budget,
        signal,
        truncated: false,
    };
    let sessions_dir = join(home_path, "sessions");
    let mut transcripts = Vec::new();
    'years: for year in newest_first(budget.read_directory(&sessions_dir)) {
        if budget.exhausted() {
            break;
        }
        for month in newest_first(budget.read_directory(&join(&sessions_dir, &year))) {
            if budget.exhausted() {
                break 'years;
            }
            for day in
                newest_first(budget.read_directory(&join(&join(&sessions_dir, &year), &month)))
            {
                if budget.exhausted() {
                    break;
                }
                let directory = join(&join(&join(&sessions_dir, &year), &month), &day);
                for entry in newest_first(budget.read_directory(&directory)) {
                    if !entry.starts_with("rollout-") || !entry.ends_with(".jsonl") {
                        continue;
                    }
                    if budget.exhausted() {
                        break;
                    }
                    if let Some(transcript) =
                        budget.stat_transcript(&join(&directory, &entry), home_path)
                    {
                        transcripts.push(transcript);
                    }
                }
            }
        }
    }
    DiscoveredTranscripts {
        transcripts,
        truncated: budget.truncated,
    }
}

/// Newest-first, giving each home a turn before taking another file from the same home.
pub fn select_metadata_transcripts(
    transcripts: &[TranscriptCandidate],
) -> Vec<TranscriptCandidate> {
    let mut grouped: Vec<(&str, Vec<&TranscriptCandidate>)> = Vec::new();
    for transcript in transcripts {
        match grouped
            .iter_mut()
            .find(|(home, _)| *home == transcript.home_path)
        {
            Some((_, list)) => list.push(transcript),
            None => grouped.push((transcript.home_path.as_str(), vec![transcript])),
        }
    }
    let mut pending: Vec<std::vec::IntoIter<&TranscriptCandidate>> = grouped
        .into_iter()
        .map(|(_, list)| list.into_iter())
        .collect();
    let mut selected = Vec::new();
    while !pending.is_empty() && selected.len() < MAX_TRANSCRIPTS_PER_SOURCE {
        let mut next_round = Vec::new();
        for mut iterator in pending {
            if selected.len() == MAX_TRANSCRIPTS_PER_SOURCE {
                break;
            }
            if let Some(next) = iterator.next() {
                selected.push(next.clone());
                next_round.push(iterator);
            }
        }
        pending = next_round;
    }
    selected
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(path: &str, home: &str) -> TranscriptCandidate {
        TranscriptCandidate {
            file_path: path.into(),
            mtime_ms: 1,
            home_path: home.into(),
            size: 1,
        }
    }

    #[test]
    fn selection_round_robins_homes() {
        let input = vec![
            candidate("a1", "A"),
            candidate("a2", "A"),
            candidate("b1", "B"),
        ];
        let order: Vec<String> = select_metadata_transcripts(&input)
            .into_iter()
            .map(|c| c.file_path)
            .collect();
        assert_eq!(order, vec!["a1", "b1", "a2"]);
    }

    #[test]
    fn discovery_walks_codex_layout_newest_first_and_claude_layout()
    -> Result<(), Box<dyn std::error::Error>> {
        use crate::session_history::session_history_fs::NodeSessionHistoryFs;
        let dir = tempfile::tempdir()?;
        let home = dir.path();
        std::fs::create_dir_all(home.join("sessions/2026/01/02"))?;
        std::fs::create_dir_all(home.join("sessions/2025/12/31"))?;
        std::fs::write(home.join("sessions/2026/01/02/rollout-b.jsonl"), "{}\n")?;
        std::fs::write(home.join("sessions/2026/01/02/other.jsonl"), "{}\n")?;
        std::fs::write(home.join("sessions/2025/12/31/rollout-a.jsonl"), "{}\n")?;
        std::fs::create_dir_all(home.join("projects/p1"))?;
        std::fs::write(home.join("projects/p1/s.jsonl"), "{}\n")?;
        let home_str = home.to_string_lossy().into_owned();
        let codex = discover_codex_transcripts(&NodeSessionHistoryFs, &home_str, 100, None);
        let names: Vec<&str> = codex
            .transcripts
            .iter()
            .map(|t| t.file_path.rsplit(['/', '\\']).next().unwrap_or(""))
            .collect();
        assert_eq!(names, vec!["rollout-b.jsonl", "rollout-a.jsonl"]);
        assert!(!codex.truncated);
        let claude = discover_claude_transcripts(&NodeSessionHistoryFs, &home_str, 100, None);
        assert_eq!(claude.transcripts.len(), 1);
        let starved = discover_claude_transcripts(&NodeSessionHistoryFs, &home_str, 0, None);
        assert!(starved.truncated && starved.transcripts.is_empty());
        let flag = AtomicBool::new(true);
        assert!(
            discover_codex_transcripts(&NodeSessionHistoryFs, &home_str, 100, Some(&flag))
                .truncated
        );
        Ok(())
    }
}
