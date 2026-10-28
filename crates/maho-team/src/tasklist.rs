//! The team tasklist (`docs/cleanroom/specs/task-control.md` §4): per-team work items that the
//! lead creates and members claim and finish, with dependencies. Records only: nothing here
//! starts or controls a child task.
//!
//! Layout: `<task state dir>/teams/<team>.tasks/` holding `<n>.json` items, `next-id` (the last
//! allocated id) and `<n>.claim` claim markers. Writes are atomic (temp file + rename); every
//! change holds an exclusive lock file on the directory, so concurrent creates get distinct ids.
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

/// A claim marker older than this is stale and may be replaced.
pub const STALE_CLAIM: Duration = Duration::from_secs(5 * 60);
/// A directory lock older than this is left over by a process that died while holding it.
const STALE_LOCK: Duration = Duration::from_secs(30);
/// How long a change waits for the directory lock.
const LOCK_WAIT: Duration = Duration::from_secs(10);
/// Longest subject or owner shown in text output.
pub const TEXT_LIMIT: usize = 80;
/// The owner name of the lead session.
pub const LEAD: &str = "lead";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemStatus {
    Pending,
    Claimed,
    InProgress,
    Completed,
    Deleted,
}

impl ItemStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Claimed => "claimed",
            Self::InProgress => "in_progress",
            Self::Completed => "completed",
            Self::Deleted => "deleted",
        }
    }
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim() {
            "pending" => Some(Self::Pending),
            "claimed" => Some(Self::Claimed),
            "in_progress" => Some(Self::InProgress),
            "completed" => Some(Self::Completed),
            "deleted" => Some(Self::Deleted),
            _ => None,
        }
    }
}

/// One work item (`<n>.json`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Item {
    pub id: u64,
    pub subject: String,
    pub description: String,
    pub status: ItemStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(default)]
    pub blocked_by: Vec<u64>,
    pub created_at: u64,
    pub updated_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claimed_at: Option<u64>,
}

/// Why an update was refused (a result, not a tool error).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    AlreadyClaimed { id: u64, owner: Option<String> },
    BlockedBy { id: u64, open: Vec<u64> },
    InvalidTransition { from: ItemStatus, to: ItemStatus },
    CrossOwner { id: u64, owner: Option<String>, caller: String },
    NotFound { id: u64 },
}

impl Refusal {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::AlreadyClaimed { .. } => "already_claimed",
            Self::BlockedBy { .. } => "blocked_by",
            Self::InvalidTransition { .. } => "invalid_transition",
            Self::CrossOwner { .. } => "cross_owner",
            Self::NotFound { .. } => "not_found",
        }
    }
    /// The one-line reason.
    pub fn reason(&self) -> String {
        match self {
            Self::AlreadyClaimed { id, owner } => match owner {
                Some(owner) => format!("Item {id} is already claimed by {}.", shorten(owner)),
                None => format!("Item {id} is not pending and cannot be claimed."),
            },
            Self::BlockedBy { id, open } => format!(
                "Item {id} is blocked by open items: {}.",
                open.iter().map(u64::to_string).collect::<Vec<_>>().join(", ")
            ),
            Self::InvalidTransition { from, to } => format!(
                "Cannot move an item from {} to {}.",
                from.as_str(),
                to.as_str()
            ),
            Self::CrossOwner { id, owner, caller } => format!(
                "Item {id} belongs to {}, not {}.",
                owner.as_deref().map_or_else(|| "nobody".to_owned(), shorten),
                shorten(caller)
            ),
            Self::NotFound { id } => format!("No item {id} in this tasklist."),
        }
    }
    pub fn details(&self) -> Value {
        let mut details = match self {
            Self::AlreadyClaimed { id, owner } => json!({"id": id, "owner": owner}),
            Self::BlockedBy { id, open } => json!({"id": id, "open": open}),
            Self::InvalidTransition { from, to } => {
                json!({"from": from.as_str(), "to": to.as_str()})
            }
            Self::CrossOwner { id, owner, caller } => {
                json!({"id": id, "owner": owner, "caller": caller})
            }
            Self::NotFound { id } => json!({"id": id}),
        };
        if let Some(map) = details.as_object_mut() {
            map.insert("kind".into(), json!(self.kind()));
        }
        details
    }
}

/// Result of an update: the item after the change, or a refusal. `Err` of the outer result is a
/// storage error.
pub type UpdateResult = Result<Result<Item, Refusal>, String>;

/// Whitespace collapsed to single spaces and cut at [`TEXT_LIMIT`] characters.
pub fn shorten(text: &str) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= TEXT_LIMIT {
        return collapsed;
    }
    let mut cut: String = collapsed.chars().take(TEXT_LIMIT - 3).collect();
    cut.push_str("...");
    cut
}

fn now_ms() -> u64 {
    maho_task::record::now_ms()
}

/// Writes `bytes` through a temp file in the same directory and a rename.
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| std::io::Error::other("path has no parent"))?;
    let temp = dir.join(format!(
        ".tmp-{}-{}",
        std::process::id(),
        uuid_like()
    ));
    let result = (|| {
        let mut file = fs::File::create(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

/// A process-unique suffix for temp files.
fn uuid_like() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!("{nanos:x}-{n}")
}

fn older_than(path: &Path, age: Duration) -> bool {
    fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
        .is_some_and(|elapsed| elapsed > age)
}

/// Creates `path` exclusively. `Ok(false)` when it exists.
fn create_exclusive(path: &Path, contents: &str) -> std::io::Result<bool> {
    match fs::OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(mut file) => {
            file.write_all(contents.as_bytes())?;
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(error) => Err(error),
    }
}

/// The exclusive directory lock, released on drop.
struct DirLock {
    path: PathBuf,
}

impl DirLock {
    fn acquire(dir: &Path) -> std::io::Result<Self> {
        let path = dir.join(".lock");
        let deadline = Instant::now() + LOCK_WAIT;
        loop {
            if create_exclusive(&path, &std::process::id().to_string())? {
                return Ok(Self { path });
            }
            if older_than(&path, STALE_LOCK) {
                let _ = fs::remove_file(&path);
                continue;
            }
            if Instant::now() >= deadline {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    format!("the tasklist {} stays locked", dir.display()),
                ));
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

impl Drop for DirLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// Filter of [`Tasklist::list`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Filter {
    pub status: Option<ItemStatus>,
    pub owner: Option<String>,
}

/// One team's tasklist directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tasklist {
    dir: PathBuf,
}

impl Tasklist {
    /// The tasklist of `team` in the task state directory `state_root`.
    pub fn new(state_root: &Path, team: &str) -> Self {
        Self {
            dir: state_root.join("teams").join(format!("{team}.tasks")),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn item_path(&self, id: u64) -> PathBuf {
        self.dir.join(format!("{id}.json"))
    }

    pub fn claim_path(&self, id: u64) -> PathBuf {
        self.dir.join(format!("{id}.claim"))
    }

    fn ensure_dir(&self) -> std::io::Result<()> {
        fs::create_dir_all(&self.dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&self.dir, fs::Permissions::from_mode(0o700))?;
        }
        Ok(())
    }

    fn save(&self, item: &Item) -> std::io::Result<()> {
        let bytes = serde_json::to_vec_pretty(item).map_err(std::io::Error::other)?;
        write_atomic(&self.item_path(item.id), &bytes)
    }

    /// The item `id`; `None` when it has no (parseable) file.
    pub fn get(&self, id: u64) -> Option<Item> {
        let bytes = fs::read(self.item_path(id)).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// Creates a `pending` item with the next id.
    ///
    /// # Errors
    /// Storage errors.
    pub fn create(&self, subject: &str, description: &str, blocked_by: Vec<u64>) -> Result<Item, String> {
        let io = |error: std::io::Error| format!("tasklist {}: {error}", self.dir.display());
        self.ensure_dir().map_err(io)?;
        let _lock = DirLock::acquire(&self.dir).map_err(io)?;
        let next_path = self.dir.join("next-id");
        let last: u64 = match fs::read_to_string(&next_path) {
            Ok(text) => text.trim().parse().map_err(|_| {
                format!("tasklist {}: next-id holds {:?}", self.dir.display(), text.trim())
            })?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
            Err(error) => return Err(io(error)),
        };
        let id = last + 1;
        write_atomic(&next_path, id.to_string().as_bytes()).map_err(io)?;
        let now = now_ms();
        let mut blocked_by = blocked_by;
        blocked_by.sort_unstable();
        blocked_by.dedup();
        let item = Item {
            id,
            subject: subject.to_owned(),
            description: description.to_owned(),
            status: ItemStatus::Pending,
            owner: None,
            blocked_by,
            created_at: now,
            updated_at: now,
            claimed_at: None,
        };
        self.save(&item).map_err(io)?;
        Ok(item)
    }

    /// Items sorted by id, filtered by exact status and owner. Files that do not parse are
    /// skipped (and named in the second value).
    pub fn list(&self, filter: &Filter) -> (Vec<Item>, Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return (Vec::new(), Vec::new());
        };
        let mut items = Vec::new();
        let mut skipped = Vec::new();
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            let Some(stem) = path
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(|name| name.strip_suffix(".json"))
            else {
                continue;
            };
            if stem.parse::<u64>().is_err() {
                continue;
            }
            match fs::read(&path)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Item>(&bytes).ok())
            {
                Some(item) => items.push(item),
                None => skipped.push(path),
            }
        }
        items.retain(|item| {
            filter.status.is_none_or(|status| item.status == status)
                && filter
                    .owner
                    .as_deref()
                    .is_none_or(|owner| item.owner.as_deref() == Some(owner))
        });
        items.sort_by_key(|item| item.id);
        skipped.sort();
        (items, skipped)
    }

    /// Ids in `blocked_by` whose item exists and is not `completed`.
    fn open_blockers(&self, item: &Item) -> Vec<u64> {
        item.blocked_by
            .iter()
            .copied()
            .filter(|id| {
                self.item_path(*id).exists()
                    && self
                        .get(*id)
                        .is_none_or(|blocker| blocker.status != ItemStatus::Completed)
            })
            .collect()
    }

    /// Moves item `id` to `status` on behalf of `caller` (spec §4.2-§4.4).
    ///
    /// # Errors
    /// Storage errors (refusals are the inner `Err`).
    pub fn update(&self, id: u64, status: ItemStatus, caller: &str) -> UpdateResult {
        let io = |error: std::io::Error| format!("tasklist {}: {error}", self.dir.display());
        if !self.dir.is_dir() {
            return Ok(Err(Refusal::NotFound { id }));
        }
        let _lock = DirLock::acquire(&self.dir).map_err(io)?;
        let Some(mut item) = self.get(id) else {
            return Ok(Err(Refusal::NotFound { id }));
        };
        let from = item.status;
        if from == status {
            // Repeating the current status is a no-op, but only the owner holds a claim.
            let owned = matches!(
                status,
                ItemStatus::Claimed | ItemStatus::InProgress | ItemStatus::Completed
            );
            if owned && item.owner.as_deref() != Some(caller) {
                return Ok(Err(if status == ItemStatus::Claimed {
                    Refusal::AlreadyClaimed { id, owner: item.owner }
                } else {
                    Refusal::CrossOwner { id, owner: item.owner, caller: caller.to_owned() }
                }));
            }
            return Ok(Ok(item));
        }
        let now = now_ms();
        if status == ItemStatus::Deleted {
            item.status = ItemStatus::Deleted;
            item.updated_at = now;
            self.save(&item).map_err(io)?;
            let _ = fs::remove_file(self.claim_path(id));
            return Ok(Ok(item));
        }
        let claim_first = from == ItemStatus::Pending
            && matches!(status, ItemStatus::Claimed | ItemStatus::InProgress);
        if claim_first {
            return self.claim(item, status, caller, now).map_err(io);
        }
        let allowed = matches!(
            (from, status),
            (ItemStatus::Claimed, ItemStatus::InProgress)
                | (ItemStatus::InProgress, ItemStatus::Completed)
        );
        if !allowed {
            if status == ItemStatus::Claimed {
                return Ok(Err(Refusal::AlreadyClaimed {
                    id,
                    owner: item.owner,
                }));
            }
            return Ok(Err(Refusal::InvalidTransition { from, to: status }));
        }
        if item.owner.as_deref() != Some(caller) {
            return Ok(Err(Refusal::CrossOwner {
                id,
                owner: item.owner,
                caller: caller.to_owned(),
            }));
        }
        item.status = status;
        item.updated_at = now;
        self.save(&item).map_err(io)?;
        Ok(Ok(item))
    }

    /// Claims a `pending` item for `caller`, then moves it on to `status` (spec §4.4).
    fn claim(
        &self,
        mut item: Item,
        status: ItemStatus,
        caller: &str,
        now: u64,
    ) -> std::io::Result<Result<Item, Refusal>> {
        let id = item.id;
        let open = self.open_blockers(&item);
        if !open.is_empty() {
            return Ok(Err(Refusal::BlockedBy { id, open }));
        }
        let marker = self.claim_path(id);
        if !create_exclusive(&marker, caller)? {
            if !older_than(&marker, STALE_CLAIM) {
                return Ok(Err(Refusal::AlreadyClaimed {
                    id,
                    owner: fs::read_to_string(&marker).ok().filter(|o| !o.is_empty()),
                }));
            }
            // A stale marker is replaced (renamed over, so two replacers cannot both win).
            let fresh = self.dir.join(format!(".claim-{}", uuid_like()));
            fs::write(&fresh, caller)?;
            fs::rename(&fresh, &marker)?;
        }
        item.status = status;
        item.owner = Some(caller.to_owned());
        item.claimed_at = Some(now);
        item.updated_at = now;
        let saved = self.save(&item);
        let _ = fs::remove_file(&marker);
        saved?;
        Ok(Ok(item))
    }
}

/// One line of the text listing.
pub fn item_line(item: &Item) -> String {
    let mut line = format!("#{} [{}] {}", item.id, item.status.as_str(), shorten(&item.subject));
    if let Some(owner) = &item.owner {
        line.push_str(&format!(" (owner {})", shorten(owner)));
    }
    if !item.blocked_by.is_empty() {
        line.push_str(&format!(
            " blocked by {}",
            item.blocked_by
                .iter()
                .map(u64::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    line
}

/// The `team_task_list` text: the count, then one line per item.
pub fn list_text(items: &[Item]) -> String {
    let mut lines = vec![format!(
        "{} item{}",
        items.len(),
        if items.len() == 1 { "" } else { "s" }
    )];
    lines.extend(items.iter().map(item_line));
    lines.join("\n")
}

/// The `team_task_get` text: every field.
pub fn item_text(item: &Item) -> String {
    let mut lines = vec![
        format!("#{} {}", item.id, shorten(&item.subject)),
        format!("status: {}", item.status.as_str()),
        format!(
            "owner: {}",
            item.owner.as_deref().map_or_else(|| "(none)".to_owned(), shorten)
        ),
        format!(
            "blocked_by: {}",
            if item.blocked_by.is_empty() {
                "(none)".to_owned()
            } else {
                item.blocked_by
                    .iter()
                    .map(u64::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        ),
        format!("created_at: {}", item.created_at),
        format!("updated_at: {}", item.updated_at),
    ];
    if let Some(claimed) = item.claimed_at {
        lines.push(format!("claimed_at: {claimed}"));
    }
    lines.push(String::new());
    lines.push(item.description.clone());
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};

    fn list() -> (tempfile::TempDir, Tasklist) {
        let dir = tempfile::tempdir().expect("tmp");
        let list = Tasklist::new(dir.path(), "crew");
        (dir, list)
    }

    fn status(list: &Tasklist, id: u64, to: ItemStatus, caller: &str) -> Result<Item, Refusal> {
        list.update(id, to, caller).expect("storage")
    }

    /// Acceptance test 10.
    #[test]
    fn ids_are_sequential_never_reused_and_distinct_under_concurrency() {
        let (_dir, list) = list();
        let ids: Vec<u64> = (0..3)
            .map(|i| list.create(&format!("s{i}"), "d", Vec::new()).expect("create").id)
            .collect();
        assert_eq!(ids, [1, 2, 3]);
        assert_eq!(status(&list, 2, ItemStatus::Deleted, LEAD).expect("delete").status, ItemStatus::Deleted);
        assert_eq!(list.create("s", "d", Vec::new()).expect("create").id, 4);
        assert_eq!(fs::read_to_string(list.dir().join("next-id")).expect("next").trim(), "4");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(list.dir()).expect("dir").permissions().mode();
            assert_eq!(mode & 0o777, 0o700);
        }

        let barrier = Arc::new(Barrier::new(10));
        let handles: Vec<_> = (0..10)
            .map(|i| {
                let list = list.clone();
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    list.create(&format!("c{i}"), "d", Vec::new()).expect("create").id
                })
            })
            .collect();
        let mut ids: Vec<u64> = handles.into_iter().map(|h| h.join().expect("thread")).collect();
        ids.sort_unstable();
        assert_eq!(ids, (5..=14).collect::<Vec<_>>());
    }

    /// Acceptance test 11.
    #[test]
    fn transitions() {
        let (_dir, list) = list();
        let a = list.create("a", "", Vec::new()).expect("create").id;
        assert_eq!(status(&list, a, ItemStatus::Claimed, "m1").expect("claim").status, ItemStatus::Claimed);
        assert_eq!(
            status(&list, a, ItemStatus::Pending, "m1"),
            Err(Refusal::InvalidTransition { from: ItemStatus::Claimed, to: ItemStatus::Pending })
        );
        assert_eq!(status(&list, a, ItemStatus::Claimed, "m1").expect("repeat").status, ItemStatus::Claimed);
        assert_eq!(status(&list, a, ItemStatus::InProgress, "m1").expect("start").status, ItemStatus::InProgress);
        assert_eq!(status(&list, a, ItemStatus::InProgress, "m1").expect("repeat").status, ItemStatus::InProgress);
        assert_eq!(status(&list, a, ItemStatus::Completed, "m1").expect("finish").status, ItemStatus::Completed);
        let refusal = status(&list, a, ItemStatus::InProgress, "m1").expect_err("back");
        assert_eq!(refusal.kind(), "invalid_transition");
        assert!(refusal.reason().contains("completed") && refusal.reason().contains("in_progress"));
        assert_eq!(status(&list, a, ItemStatus::Completed, "m1").expect("repeat").status, ItemStatus::Completed);
        assert_eq!(status(&list, a, ItemStatus::Deleted, "m1").expect("delete").status, ItemStatus::Deleted);
        assert_eq!(status(&list, a, ItemStatus::Deleted, "m1").expect("repeat").status, ItemStatus::Deleted);

        let b = list.create("b", "", Vec::new()).expect("create").id;
        assert_eq!(
            status(&list, b, ItemStatus::Completed, "m1"),
            Err(Refusal::InvalidTransition { from: ItemStatus::Pending, to: ItemStatus::Completed })
        );
        assert_eq!(status(&list, b, ItemStatus::Pending, "m1").expect("repeat").status, ItemStatus::Pending);
        assert_eq!(status(&list, b, ItemStatus::Deleted, "m1").expect("delete").status, ItemStatus::Deleted);
        let c = list.create("c", "", Vec::new()).expect("create").id;
        status(&list, c, ItemStatus::Claimed, "m1").expect("claim");
        assert_eq!(status(&list, c, ItemStatus::Deleted, "m1").expect("delete").status, ItemStatus::Deleted);
        let d = list.create("d", "", Vec::new()).expect("create").id;
        status(&list, d, ItemStatus::InProgress, "m1").expect("start");
        assert_eq!(status(&list, d, ItemStatus::Deleted, "m2").expect("delete").status, ItemStatus::Deleted);
        assert_eq!(status(&list, 99, ItemStatus::Claimed, "m1"), Err(Refusal::NotFound { id: 99 }));
    }

    /// Acceptance test 12.
    #[test]
    fn pending_to_in_progress_claims_in_one_call() {
        let (_dir, list) = list();
        let id = list.create("a", "", Vec::new()).expect("create").id;
        let item = status(&list, id, ItemStatus::InProgress, "m1").expect("start");
        assert_eq!(item.status, ItemStatus::InProgress);
        assert_eq!(item.owner.as_deref(), Some("m1"));
        assert!(item.claimed_at.is_some());
        assert_eq!(list.get(id), Some(item));
        assert!(!list.claim_path(id).exists());
    }

    /// Acceptance test 13.
    #[test]
    fn blockers() {
        let (_dir, list) = list();
        let blocker = list.create("first", "", Vec::new()).expect("create").id;
        let blocked = list.create("second", "", vec![blocker, 77]).expect("create").id;
        let refusal = status(&list, blocked, ItemStatus::Claimed, "m1").expect_err("blocked");
        assert_eq!(refusal, Refusal::BlockedBy { id: blocked, open: vec![blocker] });
        assert_eq!(refusal.details()["open"], json!([blocker]));
        assert_eq!(list.get(blocked).expect("item").status, ItemStatus::Pending);
        status(&list, blocker, ItemStatus::InProgress, "m2").expect("start");
        status(&list, blocker, ItemStatus::Completed, "m2").expect("finish");
        let item = status(&list, blocked, ItemStatus::Claimed, "m1").expect("claim");
        assert_eq!(item.owner.as_deref(), Some("m1"));

        let free = list.create("third", "", vec![1234]).expect("create").id;
        assert!(status(&list, free, ItemStatus::Claimed, "m1").is_ok());
    }

    /// Acceptance test 14.
    #[test]
    fn concurrent_claims_and_stale_markers() {
        let (_dir, list) = list();
        let id = list.create("a", "", Vec::new()).expect("create").id;
        let barrier = Arc::new(Barrier::new(2));
        let handles: Vec<_> = ["m1", "m2"]
            .into_iter()
            .map(|member| {
                let list = list.clone();
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    list.update(id, ItemStatus::Claimed, member).expect("storage")
                })
            })
            .collect();
        let results: Vec<_> = handles.into_iter().map(|h| h.join().expect("thread")).collect();
        let wins = results.iter().filter(|r| r.is_ok()).count();
        assert_eq!(wins, 1, "{results:?}");
        let loser = results.iter().find_map(|r| r.clone().err()).expect("loser");
        assert_eq!(loser.kind(), "already_claimed");

        // A fresh marker held by someone else blocks; an old one does not.
        let other = list.create("b", "", Vec::new()).expect("create").id;
        fs::write(list.claim_path(other), "m9").expect("marker");
        assert_eq!(status(&list, other, ItemStatus::Claimed, "m1").expect_err("held").kind(), "already_claimed");
        let old = SystemTime::now() - Duration::from_secs(6 * 60);
        fs::File::options()
            .write(true)
            .open(list.claim_path(other))
            .expect("open")
            .set_modified(old)
            .expect("mtime");
        let item = status(&list, other, ItemStatus::Claimed, "m1").expect("stale marker replaced");
        assert_eq!(item.owner.as_deref(), Some("m1"));
    }

    /// Acceptance test 15.
    #[test]
    fn owner_rule() {
        let (_dir, list) = list();
        let id = list.create("a", "", Vec::new()).expect("create").id;
        status(&list, id, ItemStatus::Claimed, "m1").expect("claim");
        let refusal = status(&list, id, ItemStatus::InProgress, "m2").expect_err("cross");
        assert_eq!(refusal.kind(), "cross_owner");
        assert_eq!(list.get(id).expect("item").status, ItemStatus::Claimed);
        assert_eq!(status(&list, id, ItemStatus::Deleted, "m2").expect("delete").status, ItemStatus::Deleted);
    }

    /// Acceptance test 16 (storage half; the tool half is in `tools.rs`).
    #[test]
    fn list_skips_corrupt_files_and_filters() {
        let (_dir, list) = list();
        let a = list.create("alpha", "", Vec::new()).expect("create").id;
        let b = list.create("beta", "", Vec::new()).expect("create").id;
        list.create("gamma", "", Vec::new()).expect("create");
        status(&list, a, ItemStatus::Claimed, "m1").expect("claim");
        status(&list, b, ItemStatus::Claimed, "m2").expect("claim");
        fs::write(list.item_path(10), "{ not json").expect("corrupt");
        let (all, skipped) = list.list(&Filter::default());
        assert_eq!(all.iter().map(|i| i.id).collect::<Vec<_>>(), [1, 2, 3]);
        assert_eq!(skipped, vec![list.item_path(10)]);
        let claimed = list.list(&Filter { status: Some(ItemStatus::Claimed), owner: None }).0;
        assert_eq!(claimed.len(), 2);
        let mine = list.list(&Filter { status: Some(ItemStatus::Claimed), owner: Some("m2".into()) }).0;
        assert_eq!(mine.iter().map(|i| i.id).collect::<Vec<_>>(), [b]);
        assert!(list_text(&mine).starts_with("1 item\n#2 [claimed] beta (owner m2)"));
        assert!(list.get(42).is_none());
    }

    #[test]
    fn shortening() {
        assert_eq!(shorten("  a \n\t b  "), "a b");
        let long = "x".repeat(200);
        assert_eq!(shorten(&long).chars().count(), TEXT_LIMIT);
        assert!(shorten(&long).ends_with("..."));
    }
}
