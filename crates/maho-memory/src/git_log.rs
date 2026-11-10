//! Parsing structured `git log` output into commits.
//!
//! Behaviour spec: `docs/cleanroom/specs/memory-git-log.md`. Memory history is what the store shows
//! the user when they ask what changed, so a commit that parses wrong is a false statement about the
//! past. The format is fixed by the caller's `--format` (see [`LOG_FORMAT`]).
use std::collections::BTreeMap;

/// The `--format` the parser expects, with `%x1e` between records and `%x1f` between fields.
pub const LOG_FORMAT: &str = "--format=%x1e%H%x1f%s%x1f%b%x1f%an%x1f%ae%x1f%cI";

const RECORD: char = '\x1e';
const FIELD: char = '\x1f';

/// One commit from the log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Commit {
    pub sha: String,
    pub subject: String,
    /// Line endings normalized to `\n`, trailing newlines removed.
    pub body: String,
    pub author_name: String,
    pub author_email: String,
    pub committed_at: String,
    pub trailers: BTreeMap<String, String>,
    /// `None` unless the caller asked for paths.
    pub paths: Option<Vec<String>>,
}

/// Only these are extracted. Treating every `Word: value` line as a trailer would invent metadata
/// from ordinary prose: a body line like `Note: this moved` is not a trailer.
pub const KNOWN_TRAILERS: [&str; 6] = [
    "Agent-Id",
    "Session-Id",
    "Memory-Id",
    "Operation",
    "Reason",
    "Generation",
];

fn is_known_trailer(key: &str) -> bool {
    KNOWN_TRAILERS
        .iter()
        .any(|known| known.eq_ignore_ascii_case(key))
}

fn normalize_body(raw: &str) -> String {
    let normalized = raw.replace("\r\n", "\n");
    normalized.trim_end_matches('\n').to_owned()
}

pub fn parse_trailers(body: &str) -> BTreeMap<String, String> {
    let mut trailers = BTreeMap::new();
    for line in body.lines().rev() {
        if line.trim().is_empty() {
            if trailers.is_empty() {
                continue;
            }
            break;
        }
        let Some((key, value)) = line.split_once(':') else {
            break;
        };
        let key = key.trim();
        if key.is_empty() || !is_known_trailer(key) {
            break;
        }
        trailers.insert(key.to_owned(), value.trim().to_owned());
    }
    trailers
}

pub fn parse_log(output: &str) -> Vec<Commit> {
    output
        .split(RECORD)
        .skip(1)
        .filter_map(parse_record)
        .collect()
}

fn parse_record(record: &str) -> Option<Commit> {
    let parts: Vec<&str> = record.split(FIELD).collect();
    if parts.len() < 6 {
        return None;
    }
    let body = normalize_body(parts[2]);
    Some(Commit {
        sha: parts[0].trim().to_owned(),
        subject: parts[1].to_owned(),
        trailers: parse_trailers(&body),
        body,
        author_name: parts[3].to_owned(),
        author_email: parts[4].to_owned(),
        committed_at: parts[5].trim().to_owned(),
        paths: None,
    })
}

/// Parse a `git log --name-only` output.
///
/// With `--name-only` the paths follow the last field, separated by newlines and NULs; the caller
/// emits the field separator before the date so the paths stay distinguishable from it.
pub fn parse_log_with_paths(output: &str) -> Vec<Commit> {
    output
        .split(RECORD)
        .skip(1)
        .filter_map(|record| {
            let (fields, tail) = record.rsplit_once(FIELD)?;
            let mut tail = tail.split('\0');
            let date = tail.next().unwrap_or_default();
            let mut commit = parse_record(&format!("{fields}{FIELD}{date}"))?;
            let paths: Vec<String> = tail
                .map(|path| path.strip_prefix('\n').unwrap_or(path).trim())
                .filter(|path| !path.is_empty())
                .map(str::to_owned)
                .collect();
            commit.paths = Some(paths);
            Some(commit)
        })
        .collect()
}

pub fn has_trailer(commit: &Commit, key: &str, value: &str) -> bool {
    commit
        .trailers
        .iter()
        .any(|(found, found_value)| found.eq_ignore_ascii_case(key) && found_value == value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(sha: &str, subject: &str, body: &str) -> String {
        format!(
            "{RECORD}{sha}{FIELD}{subject}{FIELD}{body}{FIELD}me{FIELD}me@example{FIELD}2026-11-10T00:00:00+09:00"
        )
    }

    fn log(body: &str) -> Vec<Commit> {
        parse_log(&record("abc123", "a subject", body))
    }

    #[test]
    fn a_single_record_parses_every_field() {
        let commits = log("body text");
        assert_eq!(commits.len(), 1);
        let commit = &commits[0];
        assert_eq!(commit.sha, "abc123");
        assert_eq!(commit.subject, "a subject");
        assert_eq!(commit.body, "body text");
        assert_eq!(commit.author_name, "me");
        assert_eq!(commit.author_email, "me@example");
        assert_eq!(commit.committed_at, "2026-11-10T00:00:00+09:00");
        assert_eq!(commit.paths, None);
    }

    #[test]
    fn several_records_keep_their_order() {
        let output = format!(
            "{}{}",
            record("a1", "first", ""),
            record("b2", "second", "")
        );
        let commits = parse_log(&output);
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0].sha, "a1");
        assert_eq!(commits[1].sha, "b2");
    }

    #[test]
    fn an_empty_output_parses_to_no_commits() {
        assert!(parse_log("").is_empty());
    }

    #[test]
    fn a_truncated_record_is_skipped_rather_than_invented() {
        let output = format!("{RECORD}abc{FIELD}subject");
        assert!(parse_log(&output).is_empty());
    }

    #[test]
    fn a_trailing_newline_is_removed_from_the_body() {
        assert_eq!(log("body\n").pop().expect("commit").body, "body");
    }

    #[test]
    fn a_windows_line_ending_is_normalized_in_the_body() {
        assert_eq!(
            log("one\r\ntwo\r\n").pop().expect("commit").body,
            "one\ntwo"
        );
    }

    #[test]
    fn internal_blank_lines_in_the_body_are_kept() {
        assert_eq!(log("one\n\ntwo").pop().expect("commit").body, "one\n\ntwo");
    }

    #[test]
    fn a_trailer_block_is_extracted() {
        let commit = log("did a thing\n\nAgent-Id: alpha\nOperation: remember")
            .pop()
            .expect("commit");
        assert_eq!(
            commit.trailers.get("Agent-Id").map(String::as_str),
            Some("alpha")
        );
        assert_eq!(
            commit.trailers.get("Operation").map(String::as_str),
            Some("remember")
        );
    }

    #[test]
    fn a_trailer_key_is_matched_case_insensitively_but_stored_as_written() {
        let commit = log("x\n\nagent-id: alpha").pop().expect("commit");
        assert_eq!(
            commit.trailers.get("agent-id").map(String::as_str),
            Some("alpha")
        );
        assert!(has_trailer(&commit, "Agent-Id", "alpha"));
    }

    #[test]
    fn an_unknown_key_is_not_a_trailer() {
        let commit = log("x\n\nRandom-Key: value").pop().expect("commit");
        assert!(commit.trailers.is_empty());
    }

    #[test]
    fn a_key_value_line_in_the_middle_of_prose_is_not_a_trailer() {
        let commit = log("Note: this moved\nmore prose").pop().expect("commit");
        assert!(commit.trailers.is_empty());
    }

    #[test]
    fn trailers_are_separated_from_a_paragraph_above_them() {
        let commit = log("prose\n\nReason: because").pop().expect("commit");
        assert_eq!(commit.trailers.len(), 1);
    }

    #[test]
    fn a_body_of_only_trailers_parses() {
        let commit = log("Agent-Id: alpha").pop().expect("commit");
        assert_eq!(commit.trailers.len(), 1);
    }

    #[test]
    fn an_empty_body_has_no_trailers() {
        assert!(log("").pop().expect("commit").trailers.is_empty());
    }

    #[test]
    fn a_trailer_value_keeps_inner_spaces_and_is_trimmed_at_the_edges() {
        let commit = log("x\n\nReason:   two words  ").pop().expect("commit");
        assert_eq!(
            commit.trailers.get("Reason").map(String::as_str),
            Some("two words")
        );
    }

    #[test]
    fn a_trailer_without_a_value_still_records_the_key() {
        let commit = log("x\n\nReason:").pop().expect("commit");
        assert_eq!(commit.trailers.get("Reason").map(String::as_str), Some(""));
    }

    #[test]
    fn paths_are_parsed_from_a_name_only_log() {
        let output = format!(
            "{RECORD}a1{FIELD}s{FIELD}b{FIELD}me{FIELD}m@e{FIELD}2026-11-10T00:00:00Z\0\nnotes/a.md\0notes/b.md\0"
        );
        let commits = parse_log_with_paths(&output);
        assert_eq!(commits.len(), 1);
        assert_eq!(
            commits[0].paths.as_deref(),
            Some(["notes/a.md".to_owned(), "notes/b.md".to_owned()].as_slice())
        );
    }

    #[test]
    fn a_commit_with_no_paths_reports_an_empty_path_list() {
        let output =
            format!("{RECORD}a1{FIELD}s{FIELD}b{FIELD}me{FIELD}m@e{FIELD}2026-11-10T00:00:00Z\0");
        let commits = parse_log_with_paths(&output);
        assert_eq!(commits[0].paths.as_deref(), Some([].as_slice()));
    }

    #[test]
    fn paths_keep_their_body_and_trailers_intact() {
        let output = format!(
            "{RECORD}a1{FIELD}s{FIELD}body{FIELD}me{FIELD}m@e{FIELD}2026-11-10T00:00:00Z\0\nnotes/a.md\0"
        );
        let commits = parse_log_with_paths(&output);
        assert_eq!(commits[0].body, "body");
        assert_eq!(
            commits[0].paths.as_deref(),
            Some(["notes/a.md".to_owned()].as_slice())
        );
    }

    #[test]
    fn a_korean_subject_and_body_survive_unchanged() {
        let commit = parse_log(&record("a1", "한국어 제목", "한국어 본문\n\nReason: 이유"))
            .pop()
            .expect("commit");
        assert_eq!(commit.subject, "한국어 제목");
        assert_eq!(
            commit.trailers.get("Reason").map(String::as_str),
            Some("이유")
        );
    }

    #[test]
    fn has_trailer_is_false_for_a_different_value() {
        let commit = log("x\n\nAgent-Id: alpha").pop().expect("commit");
        assert!(!has_trailer(&commit, "Agent-Id", "beta"));
        assert!(!has_trailer(&commit, "Operation", "alpha"));
    }
}
