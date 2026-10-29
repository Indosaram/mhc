use std::collections::BTreeMap;

use memory_core::git::{GitExec, GitExecOptions, GitMemoryRepo};
use serde::Serialize;

use super::PalaceError;

pub const HISTORY_MAX_COMMITS: usize = 500;
pub const HISTORY_RECENT_DIFFS: usize = 50;
pub const HISTORY_PER_DIFF_CAP: usize = 100_000;
pub const HISTORY_TOTAL_PAYLOAD_CAP: usize = 5_000_000;

const RECORD_SEPARATOR: char = '\u{1e}';
const GIT_TIMEOUT_MS: u64 = 30_000;
const REFLECTION_SUBJECT_PREFIXES: [&str; 4] = [
    "feat(reflection)",
    "fix(reflection)",
    "chore(reflection)",
    "merge(reflection)",
];

pub fn is_reflection_commit_subject(subject: &str) -> bool {
    REFLECTION_SUBJECT_PREFIXES
        .iter()
        .any(|prefix| subject.starts_with(prefix))
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PalaceCommit {
    pub sha: String,
    pub short_sha: String,
    pub author: String,
    pub date: String,
    pub subject: String,
    pub body: String,
    pub is_reflection: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff: Option<String>,
    pub diff_truncated: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PalaceHistoryCaps {
    pub max_commits: usize,
    pub per_diff_bytes: usize,
    pub total_diff_bytes: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PalaceHistory {
    pub commits: Vec<PalaceCommit>,
    pub caps: PalaceHistoryCaps,
}

struct CommitMetadata {
    sha: String,
    author: String,
    date: String,
    subject: String,
    body: String,
}

pub fn collect_history(repo: &GitMemoryRepo) -> Result<PalaceHistory, PalaceError> {
    let exec = repo.exec();
    collect_history_with(repo, exec.as_ref())
}

pub fn collect_history_with(
    repo: &GitMemoryRepo,
    exec: &dyn GitExec,
) -> Result<PalaceHistory, PalaceError> {
    let caps = PalaceHistoryCaps {
        max_commits: HISTORY_MAX_COMMITS,
        per_diff_bytes: HISTORY_PER_DIFF_CAP,
        total_diff_bytes: HISTORY_TOTAL_PAYLOAD_CAP,
    };
    let metadata = read_metadata(repo, exec);
    let diffs = read_diffs(repo, exec);

    let mut total_diff_bytes = 0usize;
    let mut commits = Vec::new();
    for record in metadata {
        let (diff, diff_truncated) = cap_diff(diffs.get(&record.sha).map(String::as_str), total_diff_bytes);
        total_diff_bytes += diff.as_deref().map_or(0, |value| value.chars().count());
        commits.push(PalaceCommit {
            short_sha: record.sha.chars().take(7).collect(),
            is_reflection: is_reflection_commit_subject(&record.subject),
            sha: record.sha,
            author: record.author,
            date: record.date,
            subject: record.subject,
            body: record.body,
            diff,
            diff_truncated,
        });
    }
    Ok(PalaceHistory { commits, caps })
}

fn read_metadata(repo: &GitMemoryRepo, exec: &dyn GitExec) -> Vec<CommitMetadata> {
    let limit = HISTORY_MAX_COMMITS.to_string();
    let format = format!("--format={RECORD_SEPARATOR}%H%x00%an%x00%aI%x00%s%x00%b");
    let raw = git_log(repo, exec, &["-n", limit.as_str(), "--first-parent", format.as_str()]);
    let mut records = Vec::new();
    for record in raw.split(RECORD_SEPARATOR) {
        if record.trim().is_empty() {
            continue;
        }
        let fields = record.trim_start_matches('\n').split('\0').collect::<Vec<_>>();
        if fields.len() < 4 {
            continue;
        }
        let (sha, author, date, subject) = (fields[0].trim(), fields[1].trim(), fields[2].trim(), fields[3].trim());
        if !is_full_sha(sha) {
            continue;
        }
        records.push(CommitMetadata {
            sha: sha.to_string(),
            author: author.to_string(),
            date: date.to_string(),
            subject: subject.to_string(),
            body: fields[4..].join("\0").trim().to_string(),
        });
    }
    records
}

fn read_diffs(repo: &GitMemoryRepo, exec: &dyn GitExec) -> BTreeMap<String, String> {
    let limit = HISTORY_RECENT_DIFFS.to_string();
    let format = format!("--format={RECORD_SEPARATOR}%H");
    let raw = git_log(repo, exec, &["-n", limit.as_str(), "--first-parent", format.as_str(), "-p"]);
    let mut diffs = BTreeMap::new();
    for chunk in raw.split(RECORD_SEPARATOR) {
        if chunk.trim().is_empty() {
            continue;
        }
        let normalized = chunk.trim_start_matches('\n');
        let Some(newline) = normalized.find('\n') else {
            continue;
        };
        let sha = normalized[..newline].trim();
        if !is_full_sha(sha) {
            continue;
        }
        diffs.insert(sha.to_string(), normalized[newline + 1..].to_string());
    }
    diffs
}

fn git_log(repo: &GitMemoryRepo, exec: &dyn GitExec, argv: &[&str]) -> String {
    let argv = std::iter::once("log".to_string())
        .chain(argv.iter().map(|arg| (*arg).to_string()))
        .collect::<Vec<_>>();
    let options = GitExecOptions {
        cwd: repo.dir.clone(),
        timeout_ms: GIT_TIMEOUT_MS,
        env: BTreeMap::from([("GIT_TERMINAL_PROMPT".to_string(), "0".to_string())]),
        ..GitExecOptions::default()
    };
    match exec.run(&argv, &options) {
        Ok(result) if result.code == 0 => result.stdout,
        _ => String::new(),
    }
}

fn cap_diff(diff: Option<&str>, total_so_far: usize) -> (Option<String>, bool) {
    let Some(diff) = diff else {
        return (None, false);
    };
    let mut capped = diff.to_string();
    let mut truncated = false;
    if capped.chars().count() > HISTORY_PER_DIFF_CAP {
        let head: String = capped.chars().take(HISTORY_PER_DIFF_CAP).collect();
        let kilobytes = (HISTORY_PER_DIFF_CAP as f64 / 1024.0).round() as u64;
        capped = format!("{head}\n\n[diff truncated - exceeded {kilobytes}KB]");
        truncated = true;
    }
    if total_so_far + capped.chars().count() > HISTORY_TOTAL_PAYLOAD_CAP {
        return (None, true);
    }
    (Some(capped), truncated)
}

fn is_full_sha(sha: &str) -> bool {
    sha.len() == 40 && sha.chars().all(|character| character.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::palace::test_support::create_palace_fixture;

    #[test]
    fn reflection_commits_are_tagged_and_caps_declared() {
        let mut fixture = create_palace_fixture(false);
        fixture.commit_file(
            "system/learned.md",
            "---\ndescription: learned\n---\n\nlearned thing\n",
            "feat(reflection): capture learned thing",
        );

        let history = collect_history(&fixture.repo).unwrap();

        assert!(history.commits.len() >= 2);
        let reflection = history
            .commits
            .iter()
            .find(|commit| commit.subject.starts_with("feat(reflection)"))
            .unwrap();
        assert!(reflection.is_reflection);
        assert!(reflection.diff.as_deref().unwrap_or_default().contains("learned thing"));
        assert_eq!(history.commits.iter().filter(|commit| commit.is_reflection).count(), 1);
        assert_eq!(
            history.caps,
            PalaceHistoryCaps {
                max_commits: HISTORY_MAX_COMMITS,
                per_diff_bytes: HISTORY_PER_DIFF_CAP,
                total_diff_bytes: HISTORY_TOTAL_PAYLOAD_CAP,
            }
        );
    }

    #[test]
    fn only_reflection_scoped_subjects_match() {
        assert!(is_reflection_commit_subject("feat(reflection): learn"));
        assert!(is_reflection_commit_subject("fix(reflection): repair"));
        assert!(is_reflection_commit_subject("chore(reflection): prune"));
        assert!(is_reflection_commit_subject("merge(reflection): run 12"));
        assert!(!is_reflection_commit_subject("feat(memory): unrelated"));
        assert!(!is_reflection_commit_subject("docs: mentions reflection later"));
    }
}
