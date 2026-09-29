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

            let normalized_body = raw_body.trim_end_matches(['\r', '\n']).to_string();
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
