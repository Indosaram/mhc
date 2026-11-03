//! Port of senpi `packages/coding-agent/src/core/skill-source-cache.ts`.
//!
//! Process-wide cache of user SKILL.md reads, validated by the stat the caller already took, so an
//! edited, replaced, or touched file is read again on the next load (omo#11437). Project skills never
//! come here; their checked descriptor read stays per load.

use std::collections::{HashMap, VecDeque};
use std::fs::Metadata;
use std::sync::{LazyLock, Mutex};

use crate::skill_discovery::{SKILL_FRONTMATTER_PREFIX_BYTES, read_skill_markdown_source};

/// Coarse filesystem clocks (Linux jiffies, FAT's 2 s) can give a same-size rewrite the same stamp as
/// the read before it. As git does for racy index entries, a read is reused only when the file was last
/// changed well before that read, so no later write can share its stamp. The change time is the later
/// of mtime and ctime: cp -p, tar, rsync -t and utimes restore an old mtime, and only ctime moves.
const RACY_WRITE_WINDOW_MS: f64 = 3000.0;

/// `SkillSourceCacheLimits`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SkillSourceCacheLimits {
    /// A source larger than this (no closing `---` within the frontmatter prefix) is never kept.
    pub max_entry_bytes: usize,
    /// Kept sources never exceed this in total; the oldest are dropped first.
    pub max_total_bytes: usize,
}

pub const DEFAULT_LIMITS: SkillSourceCacheLimits = SkillSourceCacheLimits {
    max_entry_bytes: 8 * SKILL_FRONTMATTER_PREFIX_BYTES,
    max_total_bytes: 8 * 1024 * 1024,
};

struct CachedSkillSource {
    stamp: String,
    source: String,
    bytes: usize,
}

#[cfg(unix)]
fn stat_identity(metadata: &Metadata) -> (u64, u64, f64, f64) {
    use std::os::unix::fs::MetadataExt;
    let ms = |secs: i64, nanos: i64| secs as f64 * 1000.0 + nanos as f64 / 1_000_000.0;
    (
        metadata.dev(),
        metadata.ino(),
        ms(metadata.mtime(), metadata.mtime_nsec()),
        ms(metadata.ctime(), metadata.ctime_nsec()),
    )
}

#[cfg(not(unix))]
fn stat_identity(metadata: &Metadata) -> (u64, u64, f64, f64) {
    let ms = |time: std::io::Result<std::time::SystemTime>| {
        time.ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0.0, |elapsed| elapsed.as_secs_f64() * 1000.0)
    };
    let modified = ms(metadata.modified());
    (0, 0, modified, modified)
}

/// `statStamp`: `dev:ino:size:mtimeMs:ctimeMs`; also returns the later change time in ms.
fn stat_stamp(metadata: &Metadata) -> (String, f64) {
    let (dev, ino, mtime, ctime) = stat_identity(metadata);
    (format!("{dev}:{ino}:{}:{mtime}:{ctime}", metadata.len()), mtime.max(ctime))
}

fn now_ms() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |elapsed| elapsed.as_secs_f64() * 1000.0)
}

#[derive(Default)]
struct Entries {
    sources: HashMap<String, CachedSkillSource>,
    /// Insertion order, oldest first.
    order: VecDeque<String>,
    total_bytes: usize,
}

impl Entries {
    fn forget(&mut self, file_path: &str) {
        if let Some(entry) = self.sources.remove(file_path) {
            self.total_bytes -= entry.bytes;
            self.order.retain(|path| path != file_path);
        }
    }
}

/// `createSkillSourceCache`: `readSkillMarkdownSource` for user skill files, reusing the last read while
/// the caller's stat still matches it.
pub struct SkillSourceCache {
    limits: SkillSourceCacheLimits,
    now: Box<dyn Fn() -> f64 + Send + Sync>,
    entries: Mutex<Entries>,
}

impl SkillSourceCache {
    pub fn new(limits: SkillSourceCacheLimits, now: impl Fn() -> f64 + Send + Sync + 'static) -> Self {
        Self { limits, now: Box::new(now), entries: Mutex::new(Entries::default()) }
    }

    pub fn read(&self, file_path: &str, metadata: &Metadata) -> Result<String, std::io::Error> {
        let (stamp, changed_ms) = stat_stamp(metadata);
        let mut entries = self.entries.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(cached) = entries.sources.get(file_path)
            && cached.stamp == stamp
        {
            return Ok(cached.source.clone());
        }
        let source = read_skill_markdown_source(file_path)?;
        entries.forget(file_path);
        // The local clock judges the file's change time, as in git; a share whose clock runs behind ours by
        // more than the window can still make a just-written file look settled.
        if changed_ms > (self.now)() - RACY_WRITE_WINDOW_MS {
            return Ok(source);
        }
        let bytes = source.len();
        if bytes > self.limits.max_entry_bytes {
            return Ok(source);
        }
        entries.sources.insert(file_path.to_owned(), CachedSkillSource { stamp, source: source.clone(), bytes });
        entries.order.push_back(file_path.to_owned());
        entries.total_bytes += bytes;
        while entries.total_bytes > self.limits.max_total_bytes {
            let Some(oldest) = entries.order.front().cloned() else { break };
            entries.forget(&oldest);
        }
        Ok(source)
    }

    pub fn len(&self) -> usize {
        self.entries.lock().unwrap_or_else(std::sync::PoisonError::into_inner).sources.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

static USER_SKILL_SOURCES: LazyLock<SkillSourceCache> = LazyLock::new(|| SkillSourceCache::new(DEFAULT_LIMITS, now_ms));

/// `readUserSkillSource`: process-wide, because a shared RPC host opens a fresh resource loader for every
/// session and each open loaded every user skill twice (omo#11437).
pub fn read_user_skill_source(file_path: &str, metadata: &Metadata) -> Result<String, std::io::Error> {
    USER_SKILL_SOURCES.read(file_path, metadata)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn clock(ms: &Arc<AtomicU64>) -> impl Fn() -> f64 + Send + Sync + 'static {
        let ms = Arc::clone(ms);
        move || ms.load(Ordering::SeqCst) as f64
    }

    fn write(path: &std::path::Path, text: &str) {
        std::fs::write(path, text).expect("write");
    }

    #[test]
    fn reuses_a_settled_read_while_the_stat_matches() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("SKILL.md");
        write(&path, "---\nname: a\n---\nbody");
        let metadata = std::fs::metadata(&path).expect("stat");
        let now = Arc::new(AtomicU64::new((now_ms() + 60_000.0) as u64));
        let cache = SkillSourceCache::new(DEFAULT_LIMITS, clock(&now));
        let key = path.to_string_lossy().into_owned();
        let first = cache.read(&key, &metadata).expect("read");
        // Same stat: the file is not read again, even though its bytes changed behind the stamp.
        std::fs::remove_file(&path).expect("remove");
        assert_eq!(cache.read(&key, &metadata).expect("cached"), first);
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn a_recent_write_is_never_kept_inside_the_racy_window() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("SKILL.md");
        write(&path, "---\nname: a\n---\nbody");
        let metadata = std::fs::metadata(&path).expect("stat");
        let now = Arc::new(AtomicU64::new(now_ms() as u64));
        let cache = SkillSourceCache::new(DEFAULT_LIMITS, clock(&now));
        cache.read(&path.to_string_lossy(), &metadata).expect("read");
        assert!(cache.is_empty());
    }

    #[test]
    fn a_changed_stamp_reads_the_file_again() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("SKILL.md");
        write(&path, "---\nname: one\n---\nbody");
        let now = Arc::new(AtomicU64::new((now_ms() + 60_000.0) as u64));
        let cache = SkillSourceCache::new(DEFAULT_LIMITS, clock(&now));
        let key = path.to_string_lossy().into_owned();
        let before = std::fs::metadata(&path).expect("stat");
        assert!(cache.read(&key, &before).expect("read").contains("name: one"));
        write(&path, "---\nname: longer text\n---\nbody");
        let after = std::fs::metadata(&path).expect("stat");
        assert!(cache.read(&key, &after).expect("read").contains("name: longer text"));
    }

    #[test]
    fn oversized_entries_are_not_kept_and_the_oldest_is_dropped_over_the_total() {
        let dir = tempfile::tempdir().expect("tempdir");
        let now = Arc::new(AtomicU64::new((now_ms() + 60_000.0) as u64));
        let limits = SkillSourceCacheLimits { max_entry_bytes: 40, max_total_bytes: 60 };
        let cache = SkillSourceCache::new(limits, clock(&now));
        let big = dir.path().join("big.md");
        write(&big, &"x".repeat(100));
        cache.read(&big.to_string_lossy(), &std::fs::metadata(&big).expect("stat")).expect("read");
        assert!(cache.is_empty());
        let paths: Vec<_> = ["a.md", "b.md"].iter().map(|name| dir.path().join(name)).collect();
        for path in &paths {
            write(path, &"y".repeat(35));
            cache.read(&path.to_string_lossy(), &std::fs::metadata(path).expect("stat")).expect("read");
        }
        assert_eq!(cache.len(), 1);
        let entries = cache.entries.lock().expect("lock");
        assert!(entries.sources.contains_key(&paths[1].to_string_lossy().into_owned()));
    }
}
