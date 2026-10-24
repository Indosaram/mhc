//! Layout under the channel's ref root (`payload-ref-paths.ts`): `gen-<generation>/refs/<refId>` and
//! `gen-<generation>/spool/`. Refs of both directions live under the child generation that owns the channel, so
//! one teardown removes all of them.

use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};

use crate::protocol::PayloadRef;
use crate::protocol_guard::is_ref_id_str;

/// `/^gen-(\d+)$/`: the generation number a directory name encodes.
fn generation_of(name: &str) -> Option<u64> {
    let digits = name.strip_prefix("gen-")?;
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

pub fn ref_generation_dir(root: &Path, generation: u64) -> PathBuf {
    root.join(format!("gen-{generation}"))
}

pub fn ref_file_path(root: &Path, ref_id: &str, generation: u64) -> io::Result<PathBuf> {
    if !is_ref_id_str(ref_id) {
        let quoted = serde_json::to_string(ref_id).unwrap_or_default();
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid ref id: {quoted}"),
        ));
    }
    Ok(ref_generation_dir(root, generation)
        .join("refs")
        .join(ref_id))
}

/// `refFilePath(root, ref)` for a full `PayloadRef`.
pub fn ref_file_path_of(root: &Path, payload_ref: &PayloadRef) -> io::Result<PathBuf> {
    ref_file_path(root, &payload_ref.ref_id, payload_ref.generation)
}

pub fn spool_dir(root: &Path, generation: u64) -> PathBuf {
    ref_generation_dir(root, generation).join("spool")
}

/// Generation teardown: every ref of that generation, acknowledged or not, and its `spool/`.
pub async fn teardown_ref_generation(root: &Path, generation: u64) -> io::Result<()> {
    match tokio::fs::remove_dir_all(ref_generation_dir(root, generation)).await {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

/// Removes the directory of every generation not in `live` (left by a dead child or server); returns them, sorted.
pub async fn sweep_ref_generations(root: &Path, live: &HashSet<u64>) -> io::Result<Vec<u64>> {
    let mut entries = match tokio::fs::read_dir(root).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut stale = Vec::new();
    while let Some(entry) = entries.next_entry().await? {
        let Some(generation) = entry.file_name().to_str().and_then(generation_of) else {
            continue;
        };
        if !live.contains(&generation) {
            stale.push(generation);
        }
    }
    stale.sort_unstable();
    for generation in &stale {
        teardown_ref_generation(root, *generation).await?;
    }
    Ok(stale)
}
