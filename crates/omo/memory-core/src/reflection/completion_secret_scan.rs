//! History-wide secret screening for a reflection branch before it is merged.

use std::collections::BTreeMap;
use std::path::Path;

use crate::git::exec::{GitExec, GitExecOptions, GitExecResult};
use crate::sync::redact::{SecretPatternClass, redact_secret_like_material, scan_secret_like_material};

use super::completion_repair::STABLE_LOG_OUTPUT;

const GIT_TIMEOUT_MS: u64 = 30_000;

/// Blob modes whose content is screened; a gitlink is refused on its file type alone.
const SCANNED_BLOB_MODES: [&str; 3] = ["100644", "100755", "120000"];
const SUBMODULE_MODE: &str = "160000";

/// One refused file version: the repository path and the refusal text, never the matched value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretLikeFailure {
    /// The repository path whose file name or content matched; the submodule path for a gitlink.
    pub path: String,
    /// The refusal text; never the matched value.
    pub detail: String,
}

/// First refusal for the range, if any (compatibility shape for callers that only need one).
pub fn find_secret_like_failure(
    exec: &dyn GitExec,
    dir: &Path,
    base_sha: &str,
    tip_sha: &str,
) -> Result<Option<String>, String> {
    Ok(find_secret_like_failures(exec, dir, base_sha, tip_sha)?
        .into_iter()
        .next()
        .map(|failure| failure.detail))
}

/// Every failing file version of every commit in the range, earliest commit first, one entry per
/// version.
///
/// The last gate before a worktree tip is merged `--no-ff` into the memory repo: every file version
/// the run's commits introduce is screened, not only the tip, because the merge carries the whole
/// branch history. Paths are scanned first (a file name is repository-controlled text like any
/// body), then every blob's full content regardless of extension. A blob already reachable from the
/// base is still scanned when the branch re-introduces it, and a non-UTF-8 blob is scanned as latin1
/// so a binary cannot smuggle an ASCII credential. The refusal names the earliest introducing commit
/// and never the matched text.
pub fn find_secret_like_failures(
    exec: &dyn GitExec,
    dir: &Path,
    base_sha: &str,
    tip_sha: &str,
) -> Result<Vec<SecretLikeFailure>, String> {
    let run = Run { exec, dir };
    let range = format!("{base_sha}..{tip_sha}");
    let rev_list = run.call(&["rev-list", "--reverse", range.as_str()])?;
    if rev_list.code != 0 {
        let stderr = rev_list.stderr.trim();
        return Err(if stderr.is_empty() {
            "git rev-list failed".to_string()
        } else {
            stderr.to_string()
        });
    }
    let mut failures = Vec::new();
    for commit in rev_list
        .stdout
        .split('\n')
        .map(|line| line.trim_end_matches('\r'))
        .filter(|line| !line.is_empty())
    {
        failures.extend(find_commit_secret_failures(&run, commit)?);
    }
    Ok(failures)
}

fn find_commit_secret_failures(
    run: &Run<'_>,
    commit: &str,
) -> Result<Vec<SecretLikeFailure>, String> {
    let records = list_commit_file_versions(run, commit)?;
    let mut failures: Vec<SecretLikeFailure> = Vec::new();
    let mut flagged: Vec<String> = Vec::new();
    let mut flag = |path: &str, detail: String| {
        if flagged.iter().any(|seen| seen == path) {
            return;
        }
        flagged.push(path.to_string());
        failures.push(SecretLikeFailure {
            path: path.to_string(),
            detail,
        });
    };
    for record in records.iter().filter(|record| record.dst_mode == SUBMODULE_MODE) {
        flag(
            &record.path,
            format!(
                "secret_like_content: {} (submodule, file type)",
                redact_secret_like_material(&record.path)
            ),
        );
    }
    let introduced: Vec<&DiffTreeRecord> = records
        .iter()
        .filter(|record| !is_zero_sha(&record.dst_sha))
        .collect();
    let short = short_sha(commit);
    for record in &introduced {
        if let Some(hit) = scan_secret_like_material(&record.path).first() {
            flag(
                &record.path,
                format!(
                    "secret_like_content: {} ({}, file name) @{short}",
                    redact_secret_like_material(&record.path),
                    class_name(hit.class)
                ),
            );
        }
    }
    let blob_records: Vec<&DiffTreeRecord> = introduced
        .iter()
        .copied()
        .filter(|record| SCANNED_BLOB_MODES.contains(&record.dst_mode.as_str()))
        .collect();
    let oids: Vec<String> = blob_records
        .iter()
        .map(|record| record.dst_sha.clone())
        .collect();
    let blobs = read_blob_contents(run, &oids)?;
    for record in blob_records {
        let Some(content) = blobs.get(&record.dst_sha) else {
            continue;
        };
        if let Some(hit) = scan_secret_like_material(content).first() {
            flag(
                &record.path,
                format!(
                    "secret_like_content: {} ({}) @{short}",
                    redact_secret_like_material(&record.path),
                    class_name(hit.class)
                ),
            );
        }
    }
    Ok(failures)
}

struct DiffTreeRecord {
    dst_mode: String,
    dst_sha: String,
    path: String,
}

/// Every file version one commit introduces, against EACH parent for merges (`-m`), including type
/// changes (`T`) whose destination blob is a new file version too - a symlink target is a blob and
/// can carry an evaded credential.
fn list_commit_file_versions(
    run: &Run<'_>,
    commit: &str,
) -> Result<Vec<DiffTreeRecord>, String> {
    let mut argv: Vec<&str> = STABLE_LOG_OUTPUT.to_vec();
    argv.extend([
        "diff-tree",
        "-r",
        "-m",
        "--no-commit-id",
        "--no-renames",
        "--diff-filter=AMT",
        "--raw",
        "-z",
        commit,
    ]);
    let result = run.call(&argv)?;
    if result.code != 0 {
        let stderr = result.stderr.trim();
        return Err(if stderr.is_empty() {
            "git diff-tree failed".to_string()
        } else {
            stderr.to_string()
        });
    }
    let parts: Vec<&str> = result.stdout.split('\0').collect();
    let mut records = Vec::new();
    let mut index = 0;
    while index + 1 < parts.len() {
        let header = parts[index];
        let path = parts[index + 1];
        index += 2;
        if path.is_empty() || !header.starts_with(':') {
            continue;
        }
        let fields: Vec<&str> = header[1..].split(' ').collect();
        if fields.len() < 5 {
            continue;
        }
        let (Some(dst_mode), Some(dst_sha)) = (fields.get(1), fields.get(3)) else {
            continue;
        };
        records.push(DiffTreeRecord {
            dst_mode: (*dst_mode).to_string(),
            dst_sha: (*dst_sha).to_string(),
            path: path.to_string(),
        });
    }
    Ok(records)
}

fn read_blob_contents(
    run: &Run<'_>,
    oids: &[String],
) -> Result<BTreeMap<String, String>, String> {
    let mut unique: Vec<&str> = Vec::new();
    for oid in oids {
        if !unique.contains(&oid.as_str()) {
            unique.push(oid.as_str());
        }
    }
    if unique.is_empty() {
        return Ok(BTreeMap::new());
    }
    let stdin = format!("{}\n", unique.join("\n"));
    let result = run.bytes(&["cat-file", "--batch"], Some(stdin.into_bytes()))?;
    if result.code != 0 {
        let stderr = result.stderr.trim();
        return Err(if stderr.is_empty() {
            "git cat-file --batch failed".to_string()
        } else {
            stderr.to_string()
        });
    }
    parse_blob_bodies(&result.stdout)
}

/// `git cat-file --batch` frames every record with `<oid> <type> <size>` followed by exactly `size`
/// bytes and a newline, so the framing has to be walked on raw bytes: a lossy decode would shift
/// every later record.
fn parse_blob_bodies(output: &[u8]) -> Result<BTreeMap<String, String>, String> {
    let mut blobs = BTreeMap::new();
    let mut offset = 0usize;
    while offset < output.len() {
        let Some(newline_offset) = output[offset..].iter().position(|byte| *byte == b'\n') else {
            return Err("git cat-file --batch output ended inside a record header".to_string());
        };
        let newline = offset + newline_offset;
        let header = String::from_utf8_lossy(&output[offset..newline]).into_owned();
        offset = newline + 1;
        let mut fields = header.split(' ');
        let oid = fields.next().unwrap_or_default();
        let kind = fields.next();
        let Some(size_text) = fields.next() else {
            continue;
        };
        let Ok(size) = size_text.parse::<usize>() else {
            return Err(format!(
                "git cat-file --batch output is malformed at \"{header}\""
            ));
        };
        if oid.is_empty() || offset + size > output.len() {
            return Err(format!(
                "git cat-file --batch output is malformed at \"{header}\""
            ));
        }
        if kind == Some("blob") {
            blobs.insert(oid.to_string(), decode_blob(&output[offset..offset + size]));
        }
        offset += size + 1;
    }
    Ok(blobs)
}

fn decode_blob(body: &[u8]) -> String {
    match std::str::from_utf8(body) {
        Ok(text) => text.to_string(),
        // Latin-1 keeps every byte distinguishable, so a binary cannot smuggle an ASCII credential.
        Err(_) => body.iter().map(|byte| *byte as char).collect(),
    }
}

fn class_name(class: SecretPatternClass) -> &'static str {
    match class {
        SecretPatternClass::AwsAccessKey => "aws_access_key",
        SecretPatternClass::CredentialAssignment => "credential_assignment",
        SecretPatternClass::AuthorizationHeader => "authorization_header",
        SecretPatternClass::OpenaiKey => "openai_key",
        SecretPatternClass::VendorToken => "vendor_token",
        SecretPatternClass::PemBlock => "pem_block",
        SecretPatternClass::SplitCredentialAssignment => "split_credential_assignment",
    }
}

fn is_zero_sha(value: &str) -> bool {
    !value.is_empty() && value.chars().all(|ch| ch == '0')
}

fn short_sha(commit: &str) -> String {
    commit.chars().take(7).collect()
}

struct Run<'a> {
    exec: &'a dyn GitExec,
    dir: &'a Path,
}

impl Run<'_> {
    fn options(&self, stdin: Option<Vec<u8>>) -> GitExecOptions {
        GitExecOptions {
            cwd: self.dir.to_path_buf(),
            timeout_ms: GIT_TIMEOUT_MS,
            env: BTreeMap::from([("GIT_TERMINAL_PROMPT".to_string(), "0".to_string())]),
            stdin,
        }
    }

    fn call(&self, argv: &[&str]) -> Result<GitExecResult, String> {
        let argv: Vec<String> = argv.iter().map(|arg| (*arg).to_string()).collect();
        self.exec
            .run(&argv, &self.options(None))
            .map_err(|error| error.to_string())
    }

    fn bytes(
        &self,
        argv: &[&str],
        stdin: Option<Vec<u8>>,
    ) -> Result<crate::git::exec::GitExecBytesResult, String> {
        let argv: Vec<String> = argv.iter().map(|arg| (*arg).to_string()).collect();
        self.exec
            .run_bytes(&argv, &self.options(stdin))
            .map_err(|error| error.to_string())
    }
}

#[cfg(test)]
#[path = "completion_secret_scan_tests.rs"]
mod tests;
