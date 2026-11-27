//! Port of senpi packages/coding-agent/src/core/session-move/inline-bundle.ts.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::bundle::{
    MANIFEST_FILE, MoveBundleInvalidError, leaves_bundle, listed_files, parse_manifest, shown,
};
use super::name_folding::{NameFolding, ProbeFoldingFn, refuse_names_that_fold};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InlineBundleFile {
    pub path: String,
    pub sha256: String,
    pub base64: String,
}

#[derive(Clone, Copy, Debug)]
pub struct InlineBundleLimits {
    pub max_bytes: u64,
    pub max_files: usize,
}

const STAGING_PREFIX: &str = "inline-";
const STALE_STAGING_AGE: Duration = Duration::from_secs(60 * 60);
const MAX_SEGMENT_BYTES: usize = 255;
const MAX_PATH_BYTES: usize = 1024;

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn segments_of(path: &str) -> Vec<&str> {
    path.split(['/', '\\']).collect()
}

fn stays_in_bundle(path: &str) -> bool {
    if leaves_bundle(path) || path.contains('\0') || path.len() > MAX_PATH_BYTES {
        return false;
    }
    segments_of(path)
        .into_iter()
        .all(|segment| !segment.is_empty() && segment != "." && segment.len() <= MAX_SEGMENT_BYTES)
}

fn refuse_file_as_folder(sent: &HashMap<String, String>) -> Result<(), MoveBundleInvalidError> {
    let files: HashSet<String> = sent.keys().map(|p| segments_of(p).join("/")).collect();
    for path in &files {
        let mut parts: Vec<&str> = path.split('/').collect();
        if !parts.is_empty() {
            parts.pop();
        }
        let mut folder = String::new();
        for seg in parts {
            if folder.is_empty() {
                folder.push_str(seg);
            } else {
                folder.push('/');
                folder.push_str(seg);
            }
            if files.contains(&folder) {
                return Err(MoveBundleInvalidError::new(format!(
                    "{} was sent as a file and as a folder",
                    shown(&folder)
                )));
            }
        }
    }
    Ok(())
}

fn sent_checksums(
    files: &[InlineBundleFile],
) -> Result<HashMap<String, String>, MoveBundleInvalidError> {
    let mut sent = HashMap::new();
    for file in files {
        if !stays_in_bundle(&file.path) {
            return Err(MoveBundleInvalidError::new(format!(
                "a sent path cannot be a file inside the bundle: {}",
                shown(&file.path)
            )));
        }
        if sent.contains_key(&file.path) {
            return Err(MoveBundleInvalidError::new(format!(
                "{} was sent twice",
                shown(&file.path)
            )));
        }
        sent.insert(file.path.clone(), file.sha256.clone());
    }
    let manifest = files
        .iter()
        .find(|f| f.path == MANIFEST_FILE)
        .ok_or_else(|| MoveBundleInvalidError::new("no manifest was sent"))?;
    let text_bytes = base64::engine::general_purpose::STANDARD
        .decode(&manifest.base64)
        .map_err(|e| MoveBundleInvalidError::new(format!("manifest base64 invalid: {e}")))?;
    if sha256_hex(&text_bytes) != manifest.sha256 {
        return Err(MoveBundleInvalidError::new(
            "the manifest changed in transit",
        ));
    }
    let text = String::from_utf8(text_bytes)
        .map_err(|e| MoveBundleInvalidError::new(format!("manifest utf8 invalid: {e}")))?;
    let parsed_manifest = parse_manifest(&text)?;
    let mut listed: HashSet<String> = listed_files(&parsed_manifest)
        .into_iter()
        .map(|f| f.path)
        .collect();
    listed.insert(MANIFEST_FILE.to_owned());
    if let Some(unlisted) = sent.keys().find(|path| !listed.contains(*path)) {
        return Err(MoveBundleInvalidError::new(format!(
            "{} was sent but the manifest does not list it",
            shown(unlisted)
        )));
    }
    refuse_file_as_folder(&sent)?;
    Ok(sent)
}

pub async fn read_inline_bundle(
    bundle_dir: &Path,
    limits: InlineBundleLimits,
) -> Result<Option<Vec<InlineBundleFile>>, String> {
    let raw_found = super::files_under(bundle_dir).await?;
    let found: Vec<String> = raw_found
        .into_iter()
        .filter_map(|file| {
            file.strip_prefix(bundle_dir)
                .ok()
                .map(|rel| rel.to_string_lossy().replace('\\', "/"))
        })
        .collect();
    if found.len() > limits.max_files {
        return Ok(None);
    }
    let mut others: Vec<String> = found.into_iter().filter(|p| p != MANIFEST_FILE).collect();
    others.sort();
    let mut paths = Vec::with_capacity(others.len() + 1);
    paths.push(MANIFEST_FILE.to_owned());
    paths.extend(others);

    let mut total_bytes = 0u64;
    for rel in &paths {
        let meta = tokio::fs::metadata(bundle_dir.join(rel))
            .await
            .map_err(|e| e.to_string())?;
        total_bytes = total_bytes.saturating_add(meta.len());
        if total_bytes > limits.max_bytes {
            return Ok(None);
        }
    }

    let mut files = Vec::with_capacity(paths.len());
    for rel in paths {
        let content = tokio::fs::read(bundle_dir.join(&rel))
            .await
            .map_err(|e| e.to_string())?;
        files.push(InlineBundleFile {
            path: rel,
            sha256: sha256_hex(&content),
            base64: base64::engine::general_purpose::STANDARD.encode(&content),
        });
    }
    Ok(Some(files))
}

async fn remove_stale_staging(incoming: &Path) -> Result<(), std::io::Error> {
    let mut rd = tokio::fs::read_dir(incoming).await?;
    let now = SystemTime::now();
    while let Some(entry) = rd.next_entry().await? {
        let name = entry.file_name();
        if !name.to_string_lossy().starts_with(STAGING_PREFIX) {
            continue;
        }
        let dir = entry.path();
        if let Ok(meta) = tokio::fs::metadata(&dir).await
            && let Ok(modified) = meta.modified()
            && let Ok(age) = now.duration_since(modified)
            && age > STALE_STAGING_AGE
        {
            let _ = tokio::fs::remove_dir_all(&dir).await;
        }
    }
    Ok(())
}

pub async fn with_staged_inline_bundle<T, F, Fut>(
    files: &[InlineBundleFile],
    incoming: &Path,
    use_bundle: F,
    probe_folding: Option<ProbeFoldingFn<'_>>,
) -> Result<T, String>
where
    F: FnOnce(PathBuf, HashMap<String, String>) -> Fut,
    Fut: Future<Output = Result<T, String>>,
{
    let sent = sent_checksums(files).map_err(|e| e.to_string())?;
    let keys: Vec<String> = sent.keys().cloned().collect();
    refuse_names_that_fold(&[&keys], incoming, probe_folding)
        .await
        .map_err(|e| e.to_string())?;
    tokio::fs::create_dir_all(incoming)
        .await
        .map_err(|e| e.to_string())?;
    let _ = remove_stale_staging(incoming).await;
    let dir = incoming.join(format!("{STAGING_PREFIX}{}", uuid::Uuid::new_v4()));
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| e.to_string())?;

    let result: Result<T, String> = async {
        for file in files {
            let mut to = dir.clone();
            for seg in file.path.split('/') {
                to.push(seg);
            }
            if let Some(parent) = to.parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .map_err(|e| e.to_string())?;
            }
            let decoded = base64::engine::general_purpose::STANDARD
                .decode(&file.base64)
                .map_err(|e| e.to_string())?;
            tokio::fs::write(&to, decoded)
                .await
                .map_err(|e| e.to_string())?;
        }
        use_bundle(dir.clone(), sent).await
    }
    .await;

    let _ = tokio::fs::remove_dir_all(&dir).await;
    result
}

pub fn name_folding_unsupported_probe() -> NameFolding {
    NameFolding {
        letter_case: false,
        unicode_form: false,
    }
}
