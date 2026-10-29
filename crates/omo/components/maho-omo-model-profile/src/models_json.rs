//! Port of omo-senpi `components/model-profile/models-json.ts` at omo `v5.1.47`: one parse of
//! `<agent_dir>/models.json` per process while the file is unchanged.
//!
//! Session start resolves several profiles and retries after auth-probe exclusions, and each pass
//! used to re-read and re-parse the file. The stamp (modification time + size) keeps an edited file
//! visible to the next session start, and a missing or unstattable file is evicted so the next read
//! reports its absence instead of a stale parse.
//!
//! The parse itself stays with the JSONC reader it already was (`utils::read_jsonc_file`), so the
//! JSONC grammar is never re-implemented here. A file that exists but does not parse is cached as
//! JSON `null`, exactly as upstream's `readJsoncFile` answers `null` without throwing; every
//! consumer already treats a non-object parse as "declares nothing".
//!
//! The cached parse is handed out behind an [`Arc`]: upstream returns the identical parsed object on
//! a cache hit (`expect(readModelsJson(dir)).toBe(first)`), and the shared handle is the native
//! equivalent of that identity.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::SystemTime;

use maho_ext_api::JsonValue;

/// Upstream `CachedModelsJson`: the parse and the stamp it was parsed under.
struct CachedModelsJson {
    stamp: FileStamp,
    parsed: Arc<JsonValue>,
}

/// Upstream `fileStamp`: `${stats.mtimeMs}:${stats.size}`, compared instead of formatted.
#[derive(Clone, Copy, PartialEq, Eq)]
struct FileStamp {
    modified: SystemTime,
    size: u64,
}

fn cache() -> &'static Mutex<HashMap<PathBuf, CachedModelsJson>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, CachedModelsJson>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Upstream `fileStamp`: `None` when the file cannot be statted.
fn file_stamp(path: &Path) -> Option<FileStamp> {
    let metadata = std::fs::metadata(path).ok()?;
    Some(FileStamp { modified: metadata.modified().ok()?, size: metadata.len() })
}

/// Upstream `readModelsJson(agentDir)`: the parsed `<agent_dir>/models.json`, cached while its
/// modification time and size are unchanged, `None` when the file is missing.
pub fn read_models_json(agent_dir: &Path) -> Option<Arc<JsonValue>> {
    let path = agent_dir.join("models.json");
    let stamp = file_stamp(&path);
    let mut cache = cache().lock().unwrap_or_else(PoisonError::into_inner);
    let Some(stamp) = stamp else {
        cache.remove(&path);
        return None;
    };
    if let Some(cached) = cache.get(&path)
        && cached.stamp == stamp
    {
        return Some(Arc::clone(&cached.parsed));
    }
    let parsed = Arc::new(utils::read_jsonc_file(&path).unwrap_or(JsonValue::Null));
    cache.insert(path, CachedModelsJson { stamp, parsed: Arc::clone(&parsed) });
    Some(parsed)
}
