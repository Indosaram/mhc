//! Idempotent frontmatter normalization to strict YAML.

use std::fs;
use std::path::{Path, PathBuf};

use crate::git::{GitCommitAuthor, GitError, GitMemoryRepo};
use crate::memfs::frontmatter::{
    MemoryFrontmatter, ParsedMemoryFile, parse_memory_file, render_memory_file,
};
use crate::memfs::frontmatter_scalar::decode_scalar_source;
use crate::memfs::frontmatter_validation::describe_frontmatter_grammar_violation;
use crate::memfs::hooks::resolve_common_git_dir;
use crate::memfs::paths::is_memory_content_path;
use crate::tools::claim_frontmatter::{
    PROVENANCE_STRIPPED_TRAILER, stamp_receipt, stripped_legacy_user_header,
};

pub const FRONTMATTER_NORMALIZATION_MARKER: &str = "omo-memory-frontmatter-normalized";
pub const FRONTMATTER_NORMALIZATION_SUBJECT: &str =
    "chore(memory): normalize frontmatter to strict YAML";

/// A skipped file during frontmatter normalization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizeFrontmatterSkip {
    pub path: String,
    pub reason: String,
}

/// The result of normalizing memory file frontmatters across a repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NormalizeFrontmatterResult {
    Clean {
        examined: usize,
        rewritten: Vec<String>,
        skipped: Vec<NormalizeFrontmatterSkip>,
    },
    Normalized {
        sha: String,
        examined: usize,
        rewritten: Vec<String>,
        skipped: Vec<NormalizeFrontmatterSkip>,
    },
    Skipped {
        detail: String,
        examined: usize,
        rewritten: Vec<String>,
        skipped: Vec<NormalizeFrontmatterSkip>,
    },
}

/// One-time, idempotent repair of legacy frontmatter to strict YAML.
pub fn normalize_memory_frontmatter(
    repo: &GitMemoryRepo,
    author: &GitCommitAuthor,
) -> Result<NormalizeFrontmatterResult, GitError> {
    let head = match repo.head()? {
        Some(h) => h,
        None => {
            return Ok(NormalizeFrontmatterResult::Clean {
                examined: 0,
                rewritten: Vec::new(),
                skipped: Vec::new(),
            });
        }
    };

    let marker_path = marker_path_for(repo);
    let checked_sha = read_marker(&marker_path);
    if checked_sha.as_deref() == Some(&head) {
        return Ok(NormalizeFrontmatterResult::Clean {
            examined: 0,
            rewritten: Vec::new(),
            skipped: Vec::new(),
        });
    }

    if !repo.status(&[] as &[&str])?.trim().is_empty() {
        return Ok(NormalizeFrontmatterResult::Skipped {
            detail: "memory repository working tree is dirty; frontmatter normalization deferred to the next clean session".to_string(),
            examined: 0,
            rewritten: Vec::new(),
            skipped: Vec::new(),
        });
    }

    let candidates = candidate_paths(repo, checked_sha.as_deref(), &head)?;
    let mut rewritten = Vec::new();
    let mut stripped = Vec::new();
    let mut skipped = Vec::new();

    for path in &candidates {
        match normalize_one(&repo.dir, path, &mut stripped) {
            NormalizeOutcome::Unchanged => {}
            NormalizeOutcome::Rewritten => rewritten.push(path.clone()),
            NormalizeOutcome::Skip(reason) => skipped.push(NormalizeFrontmatterSkip {
                path: path.clone(),
                reason,
            }),
        }
    }

    if rewritten.is_empty() {
        write_marker(&marker_path, &head).map_err(GitError::Io)?;
        return Ok(NormalizeFrontmatterResult::Clean {
            examined: candidates.len(),
            rewritten,
            skipped,
        });
    }

    let count = rewritten.len();
    let suffix = if count == 1 { "file" } else { "files" };
    let subject = format!("{FRONTMATTER_NORMALIZATION_SUBJECT} ({count} {suffix})");
    let mut msg_lines = vec![subject];
    if !stripped.is_empty() {
        msg_lines.push(String::new());
        for p in &stripped {
            msg_lines.push(format!("{PROVENANCE_STRIPPED_TRAILER}: {p}"));
        }
    }
    let message = msg_lines.join("\n");

    let commit_res = repo.commit_write(&rewritten, &message, author)?;
    write_marker(&marker_path, &commit_res.sha).map_err(GitError::Io)?;

    Ok(NormalizeFrontmatterResult::Normalized {
        sha: commit_res.sha,
        examined: candidates.len(),
        rewritten,
        skipped,
    })
}

/// True when HEAD moved past the last recorded normalization pass.
pub fn frontmatter_normalization_pending(repo: &GitMemoryRepo) -> Result<bool, GitError> {
    let head = repo.head()?;
    let marker_path = marker_path_for(repo);
    let marker_sha = read_marker(&marker_path);
    Ok(head.is_some() && marker_sha.as_deref() != head.as_deref())
}

fn marker_path_for(repo: &GitMemoryRepo) -> PathBuf {
    resolve_common_git_dir(&repo.dir).join(FRONTMATTER_NORMALIZATION_MARKER)
}

enum NormalizeOutcome {
    Unchanged,
    Rewritten,
    Skip(String),
}

fn normalize_one(root: &Path, path: &str, stripped: &mut Vec<String>) -> NormalizeOutcome {
    let absolute = root.join(path);
    let content = match fs::read_to_string(&absolute) {
        Ok(c) => c,
        Err(_) => return NormalizeOutcome::Skip("unparseable".to_string()),
    };

    if describe_frontmatter_grammar_violation(&content).is_none() {
        return NormalizeOutcome::Unchanged;
    }

    let parsed = match parse_memory_file(&content) {
        Ok(p) => p,
        Err(_) => return NormalizeOutcome::Skip("unparseable".to_string()),
    };

    if parsed.frontmatter.read_only.as_deref() == Some("true") {
        return NormalizeOutcome::Skip("read_only".to_string());
    }

    let target_fm = stamp_receipt(Some(&parsed), &parsed.frontmatter, &parsed.body, None);
    let target = ParsedMemoryFile {
        frontmatter: target_fm,
        body: parsed.body,
    };

    let rendered = match render_memory_file(&target.frontmatter, &target.body) {
        Ok(r) => r,
        Err(_) => return NormalizeOutcome::Skip("unparseable".to_string()),
    };

    if describe_frontmatter_grammar_violation(&rendered).is_some() {
        return NormalizeOutcome::Skip("not_reproducible".to_string());
    }

    let reparsed = match parse_memory_file(&rendered) {
        Ok(p) => p,
        Err(_) => return NormalizeOutcome::Skip("not_reproducible".to_string()),
    };

    if !same_memory_file(&reparsed, &target) {
        return NormalizeOutcome::Skip("not_reproducible".to_string());
    }

    if fs::write(&absolute, &rendered).is_err() {
        return NormalizeOutcome::Skip("unparseable".to_string());
    }

    if stripped_legacy_user_header(Some(&content), &rendered) {
        stripped.push(path.to_string());
    }

    NormalizeOutcome::Rewritten
}

fn same_memory_file(a: &ParsedMemoryFile, b: &ParsedMemoryFile) -> bool {
    a.body == b.body && same_frontmatter(&a.frontmatter, &b.frontmatter)
}

fn same_frontmatter(a: &MemoryFrontmatter, b: &MemoryFrontmatter) -> bool {
    if a.description != b.description || a.read_only != b.read_only || a.kind != b.kind {
        return false;
    }
    if a.aliases != b.aliases {
        return false;
    }
    let a_extra = a.extra.as_ref();
    let b_extra = b.extra.as_ref();
    let a_keys_len = a_extra.map_or(0, |e| e.len());
    let b_keys_len = b_extra.map_or(0, |e| e.len());
    if a_keys_len != b_keys_len {
        return false;
    }
    if let (Some(ae), Some(be)) = (a_extra, b_extra) {
        for (k, v) in ae {
            match be.get(k) {
                Some(bv) => {
                    if decode_scalar_source(v) != decode_scalar_source(bv) {
                        return false;
                    }
                }
                None => return false,
            }
        }
    }
    true
}

fn candidate_paths(
    repo: &GitMemoryRepo,
    checked_sha: Option<&str>,
    head: &str,
) -> Result<Vec<String>, GitError> {
    let paths = match checked_sha {
        Some(from) => match changed_since(repo, from, head) {
            Some(p) => p,
            None => repo.ls_tree(Some(head), None)?,
        },
        None => repo.ls_tree(Some(head), None)?,
    };
    Ok(paths.into_iter().filter(|p| is_memory_content_path(p)).collect())
}

fn changed_since(repo: &GitMemoryRepo, from: &str, to: &str) -> Option<Vec<String>> {
    let mut cmd = std::process::Command::new("git");
    cmd.args(["diff", "--name-only", "-z", "--diff-filter=ACMR", from, to, "--"])
        .current_dir(&repo.dir)
        .env("GIT_TERMINAL_PROMPT", "0");
    let output = cmd.output().ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    Some(
        stdout
            .split('\0')
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect(),
    )
}

fn read_marker(marker_path: &Path) -> Option<String> {
    let text = fs::read_to_string(marker_path).ok()?;
    let sha = text.trim();
    if sha.len() == 40 && sha.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(sha.to_string())
    } else {
        None
    }
}

fn write_marker(marker_path: &Path, sha: &str) -> std::io::Result<()> {
    fs::write(marker_path, format!("{sha}\n"))
}
