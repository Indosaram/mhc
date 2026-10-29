//! Git status validation and unrelated change assertions.

use std::collections::BTreeSet;
use std::path::Path;

use super::errors::GitError;
use super::porcelain::{describe_dirty_markdown_encoding_issues, parse_porcelain_path};

/// Asserts that no changes exist outside the designated target paths.
pub fn assert_no_unrelated_changes<F>(
    root: &Path,
    expected_paths: &[String],
    get_status: F,
) -> Result<(), GitError>
where
    F: FnOnce() -> Result<String, GitError>,
{
    let porcelain = get_status()?;
    if porcelain.trim().is_empty() {
        return Ok(());
    }

    let allowed: BTreeSet<&str> = expected_paths.iter().map(String::as_str).collect();
    let mut has_unrelated = false;

    for line in porcelain.lines() {
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(path) = parse_porcelain_path(trimmed)
            && !allowed.contains(path.as_str())
        {
            has_unrelated = true;
            break;
        }
    }

    if has_unrelated {
        return Err(GitError::DirtyRepo {
            porcelain: porcelain.clone(),
            encoding_diagnostics: describe_dirty_markdown_encoding_issues(root, &porcelain),
        });
    }

    Ok(())
}
