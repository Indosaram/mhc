//! Port of senpi packages/coding-agent/src/core/session-resident-store.ts.
//!
//! Large strings reachable from a session entry are replaced by a content-hash token and kept
//! resident (or spilled to a blob directory) so the in-memory mirror stays small. The JSONL file
//! remains the authority for every entry: materialize() rehydrates from the resident map, then the
//! blob directory, and otherwise leaves the token for the caller's JSONL recovery.

use std::path::Path;
use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

const RESIDENT_STRING_MIN_BYTES: usize = 32 * 1024;
const DEFAULT_RESIDENT_STRING_BUDGET_BYTES: usize = 64 * 1024 * 1024;
pub const RESIDENT_STRING_PREFIX: &str = "\u{0}senpi-resident-string:v1:";

pub type BlobsDirProvider = Arc<dyn Fn() -> Option<String> + Send + Sync>;
pub type MissingStringCallback<'a> = &'a dyn Fn(&str) -> Option<String>;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResidentStoreStats {
    pub blob_count: usize,
    pub blob_bytes: usize,
    pub evicted_count: usize,
    pub evicted_bytes: usize,
}

#[derive(Clone)]
pub struct ResidentStringStore {
    inner: Arc<Mutex<ResidentStoreInner>>,
    max_bytes: usize,
    blobs_dir: Option<BlobsDirProvider>,
}

struct ResidentStoreInner {
    strings: IndexMap<String, String>,
    bytes: usize,
    evicted_count: usize,
    evicted_bytes: usize,
}

impl Default for ResidentStringStore {
    fn default() -> Self {
        Self::new(ResidentStringStoreOptions::default())
    }
}

#[derive(Clone, Default)]
pub struct ResidentStringStoreOptions {
    pub max_bytes: Option<usize>,
    pub blobs_dir: Option<BlobsDirProvider>,
}

impl ResidentStringStore {
    pub fn new(options: ResidentStringStoreOptions) -> Self {
        Self {
            inner: Arc::new(Mutex::new(ResidentStoreInner {
                strings: IndexMap::new(),
                bytes: 0,
                evicted_count: 0,
                evicted_bytes: 0,
            })),
            max_bytes: options.max_bytes.unwrap_or(DEFAULT_RESIDENT_STRING_BUDGET_BYTES),
            blobs_dir: options.blobs_dir,
        }
    }

    pub fn configure_blobs_dir(&mut self, provider: Option<BlobsDirProvider>) {
        self.blobs_dir = provider;
    }

    pub fn clear(&self) {
        let dir = self.resolved_blobs_dir();
        {
            let mut inner = self.inner.lock().expect("resident store lock");
            inner.strings.clear();
            inner.bytes = 0;
            inner.evicted_count = 0;
            inner.evicted_bytes = 0;
        }
        if let Some(dir) = dir {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    /// Spill every resident string to the backing directory, keeping the backing for hydration.
    pub fn spill_resident(&self) {
        if self.resolved_blobs_dir().is_none() {
            return;
        }
        let mut inner = self.inner.lock().expect("resident store lock");
        let ids: Vec<String> = inner.strings.keys().cloned().collect();
        for id in ids {
            let Some(text) = inner.strings.get(&id).cloned() else { continue };
            if self.write_blob(&id, &text, &mut inner) {
                inner.strings.shift_remove(&id);
                inner.bytes = inner.bytes.saturating_sub(text.len());
            }
        }
    }

    pub fn stats(&self) -> ResidentStoreStats {
        let inner = self.inner.lock().expect("resident store lock");
        ResidentStoreStats {
            blob_count: inner.strings.len(),
            blob_bytes: inner.bytes,
            evicted_count: inner.evicted_count,
            evicted_bytes: inner.evicted_bytes,
        }
    }

    pub fn resolved_blobs_dir(&self) -> Option<String> {
        self.blobs_dir.as_ref().and_then(|provider| provider())
    }

    pub fn externalize(&self, value: &Value) -> Value {
        transform_json(value, &|text| self.externalize_string(text))
    }

    pub fn materialize(&self, value: &Value) -> Value {
        transform_json(value, &|text| self.materialize_string(text, None))
    }

    pub fn materialize_with(&self, value: &Value, on_missing: Option<MissingStringCallback<'_>>) -> Value {
        transform_json(value, &|text| self.materialize_string(text, on_missing))
    }

    pub fn externalize_in_place(&self, value: &mut Value) {
        mutate_strings_in_place(value, &|text| self.externalize_string(text));
    }

    pub fn materialize_in_place(&self, value: &mut Value) {
        mutate_strings_in_place(value, &|text| self.materialize_string(text, None));
    }

    pub fn externalize_string(&self, text: &str) -> String {
        if text.len() < RESIDENT_STRING_MIN_BYTES || text.starts_with(RESIDENT_STRING_PREFIX) {
            return text.to_owned();
        }
        let id = content_hash(text);
        let token = format!("{RESIDENT_STRING_PREFIX}{id}");
        let mut inner = self.inner.lock().expect("resident store lock");
        if inner.strings.shift_remove(&id).is_some() {
            inner.strings.insert(id, text.to_owned());
            return token;
        }
        inner.strings.insert(id, text.to_owned());
        inner.bytes += text.len();
        self.enforce_budget(&mut inner);
        token
    }

    pub fn materialize_string(&self, text: &str, on_missing: Option<MissingStringCallback<'_>>) -> String {
        let Some(id) = text.strip_prefix(RESIDENT_STRING_PREFIX) else { return text.to_owned() };
        {
            let mut inner = self.inner.lock().expect("resident store lock");
            if let Some(resident) = inner.strings.shift_remove(id) {
                let resident = resident.clone();
                inner.strings.insert(id.to_owned(), resident.clone());
                return resident;
            }
        }
        if let Some(hydrated) = self.read_blob(id) {
            return hydrated;
        }
        on_missing.and_then(|callback| callback(id)).unwrap_or_else(|| text.to_owned())
    }

    fn enforce_budget(&self, inner: &mut ResidentStoreInner) {
        while inner.bytes > self.max_bytes && !inner.strings.is_empty() {
            let Some((oldest_id, oldest)) = inner.strings.first().map(|(id, text)| (id.clone(), text.clone())) else {
                return;
            };
            if !self.write_blob(&oldest_id, &oldest, inner) {
                return;
            }
            inner.strings.shift_remove(&oldest_id);
            inner.bytes = inner.bytes.saturating_sub(oldest.len());
        }
    }

    fn write_blob(&self, id: &str, text: &str, inner: &mut ResidentStoreInner) -> bool {
        let Some(dir) = self.resolved_blobs_dir() else { return false };
        let final_path = Path::new(&dir).join(format!("{id}.blob"));
        let temp_path = Path::new(&dir).join(format!("{id}.blob.tmp"));
        if std::fs::create_dir_all(&dir).is_err() {
            return false;
        }
        if !final_path.exists() {
            let envelope = serde_json::json!({ "v": 1, "text": text });
            let Ok(serialized) = serde_json::to_string(&envelope) else { return false };
            if std::fs::write(&temp_path, serialized).is_err() || std::fs::rename(&temp_path, &final_path).is_err() {
                let _ = std::fs::remove_file(&temp_path);
                return false;
            }
        }
        inner.evicted_count += 1;
        inner.evicted_bytes += text.len();
        true
    }

    fn read_blob(&self, id: &str) -> Option<String> {
        let dir = self.resolved_blobs_dir()?;
        let file = Path::new(&dir).join(format!("{id}.blob"));
        if let Ok(content) = std::fs::read_to_string(&file)
            && let Ok(parsed) = serde_json::from_str::<Value>(&content)
                && let Some(text) = parsed.get("text").and_then(Value::as_str) {
                    return Some(text.to_owned());
                }
        let _ = std::fs::remove_file(&file);
        None
    }
}

fn content_hash(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    hex::encode(hasher.finalize())
}

fn transform_json(value: &Value, transform: &dyn Fn(&str) -> String) -> Value {
    match value {
        Value::String(text) => Value::String(transform(text)),
        Value::Number(number) => {
            if number.as_f64().map(f64::is_finite).unwrap_or(false) {
                value.clone()
            } else {
                Value::Null
            }
        }
        Value::Null | Value::Bool(_) => value.clone(),
        Value::Array(items) => Value::Array(items.iter().map(|item| transform_json(item, transform)).collect()),
        Value::Object(object) => {
            let mut transformed = Map::new();
            for (key, item) in object {
                transformed.insert(key.clone(), transform_json(item, transform));
            }
            Value::Object(transformed)
        }
    }
}

fn mutate_strings_in_place(value: &mut Value, transform: &dyn Fn(&str) -> String) {
    match value {
        Value::String(text) => *text = transform(text),
        Value::Array(items) => {
            for item in items.iter_mut() {
                mutate_strings_in_place(item, transform);
            }
        }
        Value::Object(object) => {
            let keys: Vec<String> = object.keys().cloned().collect();
            for key in keys {
                if let Some(item) = object.get_mut(&key) {
                    mutate_strings_in_place(item, transform);
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KB: usize = 1024;

    fn blob_files(dir: &str) -> Vec<String> {
        let mut files: Vec<String> = std::fs::read_dir(dir)
            .map(|entries| entries.flatten().map(|entry| entry.file_name().to_string_lossy().into_owned()).collect())
            .unwrap_or_default();
        files.sort();
        files
    }

    #[test]
    fn round_trips_large_strings_without_a_backing_directory() {
        let store = ResidentStringStore::new(ResidentStringStoreOptions { max_bytes: Some(usize::MAX), ..Default::default() });
        let text = "a".repeat(40 * KB);
        let token = store.externalize(&Value::from(text.clone()));
        assert_ne!(token, Value::from(text.clone()));
        assert_eq!(store.materialize(&token), Value::from(text));
    }

    #[test]
    fn never_evicts_without_a_recoverable_backing_directory() {
        let store = ResidentStringStore::new(ResidentStringStoreOptions { max_bytes: Some(64 * KB), ..Default::default() });
        let first = store.externalize(&Value::from("x".repeat(40 * KB)));
        let second = store.externalize(&Value::from("y".repeat(40 * KB)));
        assert_eq!(store.materialize(&first), Value::from("x".repeat(40 * KB)));
        assert_eq!(store.materialize(&second), Value::from("y".repeat(40 * KB)));
        assert_eq!(store.stats().evicted_count, 0);
    }

    #[test]
    fn evicts_to_disk_over_budget_and_hydrates_transparently() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let blobs = tmp.path().join("blobs").to_string_lossy().into_owned();
        let dir = blobs.clone();
        let store = ResidentStringStore::new(ResidentStringStoreOptions {
            max_bytes: Some(96 * KB),
            blobs_dir: Some(Arc::new(move || Some(dir.clone()))),
        });
        let texts: Vec<String> = ["a", "b", "c"].iter().map(|c| c.repeat(40 * KB)).collect();
        let tokens: Vec<Value> = texts.iter().map(|text| store.externalize(&Value::from(text.clone()))).collect();
        let stats = store.stats();
        assert!(stats.evicted_count > 0);
        assert!(stats.blob_bytes <= 96 * KB);
        assert!(blob_files(&blobs).iter().any(|file| file.ends_with(".blob")));
        for (index, token) in tokens.iter().enumerate() {
            assert_eq!(store.materialize(token), Value::from(texts[index].clone()));
        }
        assert!(store.stats().blob_bytes <= 96 * KB);
    }

    #[test]
    fn keeps_recently_used_strings_resident_and_evicts_the_oldest_first() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("blobs").to_string_lossy().into_owned();
        let provider_dir = dir.clone();
        let store = ResidentStringStore::new(ResidentStringStoreOptions {
            max_bytes: Some(80 * KB),
            blobs_dir: Some(Arc::new(move || Some(provider_dir.clone()))),
        });
        let old_text = "o".repeat(40 * KB);
        let old_token = store.externalize(&Value::from(old_text.clone()));
        store.externalize(&Value::from("n".repeat(40 * KB)));
        assert_eq!(store.materialize(&old_token), Value::from(old_text.clone()));
        store.externalize(&Value::from("t".repeat(40 * KB)));
        assert_eq!(store.materialize(&old_token), Value::from(old_text));
    }

    #[test]
    fn evict_hydrate_and_re_externalize_adds_no_blob_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("blobs").to_string_lossy().into_owned();
        let provider_dir = dir.clone();
        let store = ResidentStringStore::new(ResidentStringStoreOptions {
            max_bytes: Some(KB),
            blobs_dir: Some(Arc::new(move || Some(provider_dir.clone()))),
        });
        let text = "h".repeat(40 * KB);
        let token = store.externalize(&Value::from(text.clone()));
        let blob_files_after_eviction = blob_files(&dir).len();
        let evictions_after_first = store.stats().evicted_count;
        let hydrated = store.materialize(&token);
        let re_token = store.externalize(&hydrated);
        assert_eq!(re_token, token);
        assert_eq!(blob_files(&dir).len(), blob_files_after_eviction);
        assert_eq!(store.stats().evicted_count, evictions_after_first + 1);
    }

    #[test]
    fn stores_sharing_one_backing_directory_never_hydrate_each_others_text() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("blobs").to_string_lossy().into_owned();
        let dir_a = dir.clone();
        let dir_b = dir.clone();
        let store_a = ResidentStringStore::new(ResidentStringStoreOptions {
            max_bytes: Some(KB),
            blobs_dir: Some(Arc::new(move || Some(dir_a.clone()))),
        });
        let store_b = ResidentStringStore::new(ResidentStringStoreOptions {
            max_bytes: Some(KB),
            blobs_dir: Some(Arc::new(move || Some(dir_b.clone()))),
        });
        let text_a = "a".repeat(40 * KB);
        let text_b = "b".repeat(40 * KB);
        let token_a = store_a.externalize(&Value::from(text_a.clone()));
        let token_b = store_b.externalize(&Value::from(text_b.clone()));
        assert_eq!(store_a.materialize(&token_a), Value::from(text_a));
        assert_eq!(store_b.materialize(&token_b), Value::from(text_b));
    }

    #[test]
    fn keeps_strings_resident_when_the_backing_directory_is_unwritable() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let blocker = tmp.path().join("blocker");
        std::fs::write(&blocker, "not a directory").expect("write");
        let unwritable = blocker.join("blobs").to_string_lossy().into_owned();
        let store = ResidentStringStore::new(ResidentStringStoreOptions {
            max_bytes: Some(KB),
            blobs_dir: Some(Arc::new(move || Some(unwritable.clone()))),
        });
        let text = "k".repeat(40 * KB);
        let token = store.externalize(&Value::from(text.clone()));
        assert_eq!(store.materialize(&token), Value::from(text));
        assert_eq!(store.stats().blob_bytes, 40 * KB);
    }

    #[test]
    fn spills_resident_strings_to_the_backing_and_keeps_it_for_hydration() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("blobs").to_string_lossy().into_owned();
        let provider_dir = dir.clone();
        let store = ResidentStringStore::new(ResidentStringStoreOptions {
            max_bytes: Some(usize::MAX),
            blobs_dir: Some(Arc::new(move || Some(provider_dir.clone()))),
        });
        let text = "s".repeat(40 * KB);
        let token = store.externalize(&Value::from(text.clone()));
        store.spill_resident();
        assert_eq!(store.stats().blob_count, 0);
        assert_eq!(store.stats().blob_bytes, 0);
        assert_eq!(store.materialize(&token), Value::from(text));
        assert_eq!(blob_files(&dir).len(), 1);
    }

    #[test]
    fn hydrates_readable_but_corrupt_blobs_by_falling_back_to_the_caller() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("blobs").to_string_lossy().into_owned();
        let provider_dir = dir.clone();
        let store = ResidentStringStore::new(ResidentStringStoreOptions {
            max_bytes: Some(KB),
            blobs_dir: Some(Arc::new(move || Some(provider_dir.clone()))),
        });
        let text = "c".repeat(40 * KB);
        let token = store.externalize(&Value::from(text.clone()));
        let id = token.as_str().unwrap_or_default().trim_start_matches(RESIDENT_STRING_PREFIX).to_owned();
        std::fs::write(std::path::Path::new(&dir).join(format!("{id}.blob")), "{}").expect("write");
        let recovered = store.materialize_with(&token, Some(&|_| Some(text.clone())));
        assert_eq!(recovered, Value::from(text));
    }

    #[test]
    fn re_externalizing_a_spilled_string_makes_it_resident_again_with_the_same_token() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("blobs").to_string_lossy().into_owned();
        let provider_dir = dir.clone();
        let store = ResidentStringStore::new(ResidentStringStoreOptions {
            max_bytes: Some(usize::MAX),
            blobs_dir: Some(Arc::new(move || Some(provider_dir.clone()))),
        });
        let text = "r".repeat(40 * KB);
        let token = store.externalize(&Value::from(text.clone()));
        store.spill_resident();
        let hydrated = store.materialize(&token);
        assert_eq!(store.externalize(&hydrated), token);
        assert_eq!(store.stats().blob_count, 1);
    }

    #[test]
    fn a_corrupt_blob_is_removed_on_read_so_the_next_eviction_rewrites_it() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("blobs").to_string_lossy().into_owned();
        let provider_dir = dir.clone();
        let store = ResidentStringStore::new(ResidentStringStoreOptions {
            max_bytes: Some(KB),
            blobs_dir: Some(Arc::new(move || Some(provider_dir.clone()))),
        });
        let text = "z".repeat(40 * KB);
        let token = store.externalize(&Value::from(text.clone()));
        let id = token.as_str().unwrap_or_default().trim_start_matches(RESIDENT_STRING_PREFIX).to_owned();
        let file = std::path::Path::new(&dir).join(format!("{id}.blob"));
        std::fs::write(&file, "garbage").expect("write");
        assert!(store.materialize_with(&token, Some(&|_| None)).as_str().unwrap_or_default().starts_with(RESIDENT_STRING_PREFIX));
        assert!(!file.exists());
    }

    #[test]
    fn leaves_strings_resident_on_spill_without_a_backing_directory() {
        let store = ResidentStringStore::new(ResidentStringStoreOptions { max_bytes: Some(usize::MAX), ..Default::default() });
        let text = "p".repeat(40 * KB);
        let token = store.externalize(&Value::from(text.clone()));
        store.spill_resident();
        assert_eq!(store.materialize(&token), Value::from(text));
    }

    #[test]
    fn externalize_and_materialize_in_place_round_trip_runtime_state() {
        let store = ResidentStringStore::new(ResidentStringStoreOptions { max_bytes: Some(usize::MAX), ..Default::default() });
        let text = "m".repeat(40 * KB);
        let mut value = serde_json::json!({ "a": { "b": [text.clone()] }, "n": 1 });
        store.externalize_in_place(&mut value);
        assert!(value["a"]["b"][0].as_str().unwrap_or_default().starts_with(RESIDENT_STRING_PREFIX));
        store.materialize_in_place(&mut value);
        assert_eq!(value["a"]["b"][0], Value::from(text));
        assert_eq!(value["n"], 1);
    }

    #[test]
    fn removes_blob_files_on_clear() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("blobs").to_string_lossy().into_owned();
        let provider_dir = dir.clone();
        let store = ResidentStringStore::new(ResidentStringStoreOptions {
            max_bytes: Some(KB),
            blobs_dir: Some(Arc::new(move || Some(provider_dir.clone()))),
        });
        store.externalize(&Value::from("q".repeat(40 * KB)));
        assert!(!blob_files(&dir).is_empty());
        store.clear();
        assert!(!std::path::Path::new(&dir).exists());
        assert_eq!(store.stats(), ResidentStoreStats::default());
    }

    #[test]
    fn short_strings_and_already_resident_tokens_pass_through() {
        let store = ResidentStringStore::new(ResidentStringStoreOptions::default());
        assert_eq!(store.externalize(&Value::from("small")), Value::from("small"));
        let token = format!("{RESIDENT_STRING_PREFIX}deadbeef");
        assert_eq!(store.externalize(&Value::from(token.clone())), Value::from(token));
    }

    #[test]
    fn round_trips_surrogate_pairs_through_eviction() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("blobs").to_string_lossy().into_owned();
        let provider_dir = dir.clone();
        let store = ResidentStringStore::new(ResidentStringStoreOptions {
            max_bytes: Some(KB),
            blobs_dir: Some(Arc::new(move || Some(provider_dir.clone()))),
        });
        let text = "\u{1f600}".repeat(20 * KB);
        let token = store.externalize(&Value::from(text.clone()));
        assert_eq!(store.materialize(&token), Value::from(text));
    }

    #[test]
    fn transforms_nested_structures_and_preserves_non_strings() {
        let store = ResidentStringStore::new(ResidentStringStoreOptions { max_bytes: Some(usize::MAX), ..Default::default() });
        let text = "n".repeat(40 * KB);
        let value = serde_json::json!({ "list": [text.clone(), 1, true, null], "inner": { "s": text.clone() } });
        let externalized = store.externalize(&value);
        let round_tripped = store.materialize(&externalized);
        assert_eq!(round_tripped, value);
    }
}
