//! Git log output and diff-tree path parsing.

use std::collections::BTreeMap;

use super::repo_types::MemoryCommit;

/// Parses structured Git log output into a sequence of MemoryCommit records.
pub fn parse_log_output(output: &str) -> Vec<MemoryCommit> {
    output
        .split('\x1e')
        .skip(1)
        .map(|record| {
            let parts: Vec<&str> = record.split('\x1f').collect();
            let sha = parts.first().copied().unwrap_or_default().to_string();
            let subject = parts.get(1).copied().unwrap_or_default().to_string();
            let raw_body = parts.get(2).copied().unwrap_or_default();
            let author_name = parts.get(3).copied().unwrap_or_default().to_string();
            let author_email = parts.get(4).copied().unwrap_or_default().to_string();
            let committed_at = parts.get(5).copied().unwrap_or_default().trim().to_string();

            let normalized_body = raw_body
                .strip_suffix("\r\n")
                .or_else(|| raw_body.strip_suffix('\n'))
                .unwrap_or(raw_body)
                .to_string();
            let trailers = parse_trailers(&normalized_body);

            MemoryCommit {
                sha,
                subject,
                body: normalized_body,
                author_name,
                author_email,
                committed_at,
                trailers,
                paths: None,
            }
        })
        .collect()
}

/// Parses paths carried in the same log process, preserving whitespace and embedded newlines.
pub fn parse_log_output_with_paths(output: &str) -> Vec<MemoryCommit> {
    output
        .split('\x1e')
        .skip(1)
        .filter_map(|record| {
            let (fields, tail) = record.rsplit_once('\x1f')?;
            let mut tail = tail.split('\0');
            let date = tail.next().unwrap_or_default();
            let mut commit = parse_log_output(&format!("\x1e{fields}\x1f{date}")).pop()?;
            commit.paths = Some(
                tail.map(|path| path.strip_prefix('\n').unwrap_or(path))
                    .filter(|path| !path.is_empty())
                    .map(str::to_string)
                    .collect(),
            );
            Some(commit)
        })
        .collect()
}

/// Parses NUL-separated path listings into a list of file path strings.
pub fn parse_nul_paths(output: &str) -> Vec<String> {
    output
        .split('\0')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
        .collect()
}

fn parse_trailers(body: &str) -> BTreeMap<String, String> {
    let mut trailers = BTreeMap::new();
    for line in body.lines() {
        let trimmed = line.trim_end();
        if let Some((raw_key, raw_val)) = trimmed.split_once(':') {
            if raw_key.starts_with(' ') || raw_key.starts_with('\t') || raw_key.is_empty() {
                continue;
            }
            let key = raw_key.trim();
            if !key.is_empty() {
                let value = raw_val.strip_prefix(' ').unwrap_or(raw_val).to_string();
                trailers.insert(key.to_string(), value);
            }
        }
    }
    trailers
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn paths_preserve_spaces_newlines_and_empty_commits() {
        let commits = parse_log_output_with_paths(
            "\x1esha\x1fsubject\x1fbody\n\x1fA\x1fa@b\x1fdate\0\n spaced .md\0line\nname.md\0\x1eempty\x1fe\x1f\x1fA\x1fa@b\x1fdate\0",
        );
        assert_eq!(
            commits[0].paths.as_ref().unwrap(),
            &[" spaced .md", "line\nname.md"]
        );
        assert_eq!(commits[1].paths, Some(vec![]));
    }
}
