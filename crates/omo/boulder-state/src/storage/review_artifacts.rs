//! Momus review artifacts beside a plan (`storage/review-artifacts.ts`).

use std::fs;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

use chrono::{DateTime, SecondsFormat, Utc};
use sha2::{Digest, Sha256};

use super::path::resolve_tracked_path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MomusReviewVerdict {
    Okay,
    Reject,
}

impl MomusReviewVerdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Okay => "okay",
            Self::Reject => "reject",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct MomusReviewArtifactInput {
    pub verdict: MomusReviewVerdict,
    pub reviewed_at: DateTime<Utc>,
    pub model: Option<String>,
    pub plan_hash: Option<String>,
    pub duration_ms: Option<f64>,
    pub session_id: Option<String>,
    pub reviewer_output: String,
}

const MAX_SEQUENCE_ATTEMPTS: usize = 1000;

/// `^(\d+)-`: the leading sequence number of a review file name.
fn review_file_sequence(entry: &str) -> Option<u64> {
    let (digits, _) = entry.split_once('-')?;
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    // Upstream `Number.parseInt` never fails on a digit run; a run too long for u64 saturates.
    Some(digits.parse::<u64>().unwrap_or(u64::MAX))
}

fn resolve_plan_file_path(directory: &Path, plan_path: &str) -> PathBuf {
    resolve_tracked_path(directory, Path::new(plan_path))
}

/// `<plan without .md>.reviews`, resolved against `directory`.
pub fn get_plan_reviews_dir(directory: &Path, plan_path: &str) -> PathBuf {
    let plan_file_path = resolve_plan_file_path(directory, plan_path);
    let text = plan_file_path.to_string_lossy();
    let without_extension = text.strip_suffix(".md").unwrap_or(&text);
    PathBuf::from(format!("{without_extension}.reviews"))
}

/// Hex sha256 of the plan text.
pub fn hash_plan_content(content: &str) -> String {
    hex::encode(Sha256::digest(content.as_bytes()))
}

fn format_duration(duration_ms: Option<f64>) -> String {
    match duration_ms {
        Some(ms) if ms.is_finite() && ms >= 0.0 => format!("{:.1}s", ms / 1000.0),
        _ => "unknown".to_string(),
    }
}

pub fn format_momus_review_artifact(
    sequence: u64,
    plan_path: &str,
    artifact: &MomusReviewArtifactInput,
) -> String {
    let mut lines = vec![
        format!(
            "# Momus review {sequence:03}: {}",
            artifact.verdict.as_str().to_uppercase()
        ),
        String::new(),
        format!("- Plan: {plan_path}"),
        format!("- Verdict: {}", artifact.verdict.as_str()),
        format!(
            "- Reviewed at: {}",
            artifact
                .reviewed_at
                .to_rfc3339_opts(SecondsFormat::Millis, true)
        ),
        format!(
            "- Model: {}",
            artifact.model.as_deref().unwrap_or("unknown")
        ),
        format!(
            "- Plan hash (sha256): {}",
            artifact.plan_hash.as_deref().unwrap_or("unavailable")
        ),
        format!("- Duration: {}", format_duration(artifact.duration_ms)),
    ];
    if let Some(session_id) = artifact.session_id.as_deref().filter(|id| !id.is_empty()) {
        lines.push(format!("- Reviewer session: {session_id}"));
    }
    lines.extend([
        String::new(),
        "## Reviewer output".to_string(),
        String::new(),
        artifact.reviewer_output.trim().to_string(),
        String::new(),
    ]);
    lines.join("\n")
}

/// `path.relative(root, candidate)` is `""` or does not climb out: component-wise prefix.
fn is_inside_directory(root: &Path, candidate: &Path) -> bool {
    candidate.starts_with(root)
}

/// Real path of the longest existing prefix, with the missing tail re-joined.
fn realpath_with_missing_tail(path: &Path) -> PathBuf {
    let mut current = path.to_path_buf();
    let mut missing_tail: Vec<std::ffi::OsString> = Vec::new();
    loop {
        if let Ok(real) = fs::canonicalize(&current) {
            let mut result = real;
            for part in missing_tail.iter().rev() {
                result.push(part);
            }
            return result;
        }
        let (Some(parent), Some(name)) = (current.parent(), current.file_name()) else {
            return resolve_tracked_path(Path::new(""), path);
        };
        missing_tail.push(name.to_os_string());
        current = parent.to_path_buf();
    }
}

struct ContainedPlan {
    plans_root: PathBuf,
    reviews_dir: PathBuf,
}

fn resolve_contained_plan(directory: &Path, plan_path: &str) -> Option<ContainedPlan> {
    let plans_root = resolve_tracked_path(directory, &Path::new(".omo").join("plans"));
    let plan_file_path = resolve_plan_file_path(directory, plan_path);
    if !is_inside_directory(&plans_root, &plan_file_path) {
        return None;
    }
    let real_plans_root = realpath_with_missing_tail(&plans_root);
    let real_directory =
        realpath_with_missing_tail(&resolve_tracked_path(directory, Path::new("")));
    if !is_inside_directory(&real_directory, &real_plans_root) {
        return None;
    }
    let real_plan_file_path = realpath_with_missing_tail(&plan_file_path);
    if !is_inside_directory(&real_plans_root, &real_plan_file_path) {
        return None;
    }
    Some(ContainedPlan {
        plans_root,
        reviews_dir: get_plan_reviews_dir(directory, plan_path),
    })
}

/// The plan resolves inside `<directory>/.omo/plans`, also after following symlinks.
pub fn is_contained_plan_path(directory: &Path, plan_path: &str) -> bool {
    resolve_contained_plan(directory, plan_path).is_some()
}

fn find_artifact_by_session_id(
    reviews_dir: &Path,
    session_id: &str,
) -> std::io::Result<Option<PathBuf>> {
    let session_line = format!("- Reviewer session: {session_id}");
    for entry in fs::read_dir(reviews_dir)? {
        let entry = entry?;
        let name = entry.file_name();
        if review_file_sequence(&name.to_string_lossy()).is_none() {
            continue;
        }
        let path = reviews_dir.join(&name);
        let Ok(content) = fs::read_to_string(&path) else {
            continue;
        };
        if content.split('\n').any(|line| line == session_line) {
            return Ok(Some(path));
        }
    }
    Ok(None)
}

fn next_review_sequence(reviews_dir: &Path) -> std::io::Result<u64> {
    if !reviews_dir.exists() {
        return Ok(1);
    }
    let mut max = 0;
    for entry in fs::read_dir(reviews_dir)? {
        if let Some(value) = review_file_sequence(&entry?.file_name().to_string_lossy()) {
            max = max.max(value);
        }
    }
    Ok(max.saturating_add(1))
}

/// `O_WRONLY | O_CREAT | O_EXCL`: with `O_EXCL`, an existing path (a symlink included) is never
/// opened, so a planted symlink cannot redirect the write; it only advances the sequence.
fn create_artifact_file(
    reviews_dir: &Path,
    plan_path: &str,
    artifact: &MomusReviewArtifactInput,
) -> std::io::Result<PathBuf> {
    let mut sequence = next_review_sequence(reviews_dir)?;
    for _ in 0..MAX_SEQUENCE_ATTEMPTS {
        let file_path = reviews_dir.join(format!("{sequence:03}-{}.md", artifact.verdict.as_str()));
        let mut file = match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&file_path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {
                sequence += 1;
                continue;
            }
            Err(error) => return Err(error),
        };
        file.write_all(format_momus_review_artifact(sequence, plan_path, artifact).as_bytes())?;
        return Ok(file_path);
    }
    Err(std::io::Error::other(format!(
        "Could not allocate a review artifact sequence in {}",
        reviews_dir.display()
    )))
}

fn ensure_plans_gitignore(plans_root: &Path) {
    let gitignore_path = plans_root.join(".gitignore");
    if gitignore_path.exists() {
        return;
    }
    // Best effort only: a missing ignore file must never block the artifact write.
    let _ = fs::write(gitignore_path, "*.reviews\n");
}

/// Writes `NNN-<verdict>.md` into the plan's `.reviews` directory. `Ok(None)` when the plan (or
/// its reviews directory) is not contained in the project's plans root; a reviewer session that
/// was already persisted returns its existing file.
pub fn write_momus_review_artifact(
    directory: &Path,
    plan_path: &str,
    artifact: &MomusReviewArtifactInput,
) -> std::io::Result<Option<PathBuf>> {
    let Some(ContainedPlan {
        plans_root,
        reviews_dir,
    }) = resolve_contained_plan(directory, plan_path)
    else {
        return Ok(None);
    };
    if !reviews_dir.exists() {
        fs::create_dir_all(&reviews_dir)?;
    }
    ensure_plans_gitignore(&plans_root);
    if !is_inside_directory(
        &fs::canonicalize(&plans_root)?,
        &fs::canonicalize(&reviews_dir)?,
    ) {
        return Ok(None);
    }
    if let Some(session_id) = artifact.session_id.as_deref().filter(|id| !id.is_empty())
        && let Some(existing) = find_artifact_by_session_id(&reviews_dir, session_id)?
    {
        return Ok(Some(existing));
    }
    create_artifact_file(&reviews_dir, plan_path, artifact).map(Some)
}
