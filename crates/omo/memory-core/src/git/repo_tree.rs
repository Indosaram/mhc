//! Parsers for `git ls-tree` and `git cat-file --batch` output.

use std::collections::BTreeMap;

use super::errors::GitError;
use super::repo_types::{GitTreeBlobEntry, GitTreeSizedEntry};

/// Parses `git ls-tree -r -l -z` records into sized entries, skipping non-blob (`-`) sizes.
pub fn parse_ls_tree_sized(stdout: &str) -> Vec<GitTreeSizedEntry> {
    let mut entries = Vec::new();
    for record in stdout.split('\0') {
        if record.is_empty() {
            continue;
        }
        let Some(tab) = record.find('\t') else {
            continue;
        };
        let meta: Vec<&str> = record[..tab].split_whitespace().collect();
        let path = &record[tab + 1..];
        if meta.len() < 4 || path.is_empty() {
            continue;
        }
        let size = meta[3];
        if size == "-" {
            continue;
        }
        let Ok(bytes) = size.parse::<u64>() else {
            continue;
        };
        entries.push(GitTreeSizedEntry {
            path: path.to_string(),
            bytes,
        });
    }
    entries
}

/// Parses `git ls-tree -r -z` records into blob entries with their object ids.
pub fn parse_ls_tree_blobs(stdout: &str) -> Vec<GitTreeBlobEntry> {
    let mut entries = Vec::new();
    for record in stdout.split('\0') {
        let Some(tab) = record.find('\t') else {
            continue;
        };
        let meta: Vec<&str> = record[..tab].split_whitespace().collect();
        let path = &record[tab + 1..];
        if meta.len() < 3 || meta[1] != "blob" || path.is_empty() {
            continue;
        }
        entries.push(GitTreeBlobEntry {
            path: path.to_string(),
            oid: meta[2].to_string(),
        });
    }
    entries
}

/// Parses `<oid> <type> <size>\n<content>\n` records; `<oid> missing` records carry no content.
pub fn parse_cat_file_batch(output: &[u8]) -> Result<BTreeMap<String, String>, GitError> {
    let mut blobs = BTreeMap::new();
    let mut offset = 0usize;
    while offset < output.len() {
        let Some(relative_newline) = output[offset..].iter().position(|byte| *byte == b'\n') else {
            return Err(GitError::Other(
                "git cat-file --batch output ended inside a record header".to_string(),
            ));
        };
        let newline = offset + relative_newline;
        let header = String::from_utf8_lossy(&output[offset..newline]).into_owned();
        offset = newline + 1;
        let mut parts = header.split(' ');
        let oid = parts.next();
        let kind = parts.next();
        let Some(size_text) = parts.next() else {
            continue;
        };
        let malformed =
            || GitError::Other(format!("git cat-file --batch output is malformed at \"{header}\""));
        let size = size_text.parse::<usize>().map_err(|_| malformed())?;
        let oid = oid.ok_or_else(malformed)?;
        if offset + size > output.len() {
            return Err(malformed());
        }
        if kind == Some("blob") {
            blobs.insert(
                oid.to_string(),
                String::from_utf8_lossy(&output[offset..offset + size]).into_owned(),
            );
        }
        offset += size + 1;
    }
    Ok(blobs)
}

#[cfg(test)]
#[path = "repo_tree_tests.rs"]
mod tests;
