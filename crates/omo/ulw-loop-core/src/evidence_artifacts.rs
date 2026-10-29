//! Evidence artifacts validation and path normalization.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::paths::repo_relative;
use crate::runtime::UlwLoopError;

fn fail(message: impl Into<String>, code: impl Into<String>, details: BTreeMap<String, serde_json::Value>) -> Result<Vec<String>, UlwLoopError> {
    Err(UlwLoopError::with_details(message, code, details))
}

fn stored(repo_root: &Path, absolute: &Path) -> String {
    let repo_str = repo_root.to_string_lossy();
    let abs_str = absolute.to_string_lossy();
    repo_relative(&abs_str, &repo_str)
}

pub fn resolve_evidence_artifacts(
    repo_root: impl AsRef<Path>,
    artifacts: Option<&[String]>,
) -> Result<Option<Vec<String>>, UlwLoopError> {
    let Some(arts) = artifacts else {
        return Ok(None);
    };

    let root = repo_root.as_ref();
    let mut seen = BTreeSet::new();
    let mut resolved = Vec::new();

    for (index, candidate) in arts.iter().enumerate() {
        let path = candidate.trim();
        if path.is_empty() {
            let mut details = BTreeMap::new();
            details.insert("index".into(), serde_json::json!(index));
            return fail(
                format!("Artifact {} must be a non-empty path.", index + 1),
                "ULW_LOOP_ARGUMENT_INVALID",
                details,
            ).map(Some);
        }

        let absolute = if Path::new(path).is_absolute() {
            PathBuf::from(path)
        } else {
            root.join(path)
        };

        if !absolute.exists() {
            let mut details = BTreeMap::new();
            details.insert("path".into(), serde_json::json!(path));
            details.insert("resolved".into(), serde_json::json!(absolute.to_string_lossy()));
            return fail(
                format!(
                    "Evidence artifact does not exist: {} (resolved to {}).",
                    path,
                    absolute.display()
                ),
                "ULW_LOOP_EVIDENCE_ARTIFACT_MISSING",
                details,
            ).map(Some);
        }

        let value = stored(root, &absolute);
        if seen.insert(value.clone()) {
            resolved.push(value);
        }
    }

    Ok(Some(resolved))
}
