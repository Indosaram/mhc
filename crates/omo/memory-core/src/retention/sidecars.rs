//! Kibitzer sidecar retention (`recall/sidecars/<encoded-session>/`, design 2.4).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use crate::fs::remove_tree::remove_tree;
use crate::locks::acquire::{AcquireLockError, AcquireLockOptions, acquire_lock, release_lock};
use crate::locks::lock_record::create_lock_record;
use crate::support::random::random_uuid;

use super::policy::{RetentionKeepReason, list_names};

pub const KIBITZER_SIDECAR_OWNER_LOCK_PURPOSE: &str = "recall-sidecar";
pub const KIBITZER_SIDECAR_TOMBSTONE_PREFIX: &str = ".prune-";
pub const KIBITZER_ENCODED_SESSION_PATTERN: &str = "^[A-Za-z0-9_-]+$";
const SIDECARS_DIRNAME: &str = "sidecars";
const OWNER_LOCK_PREFIX: &str = "recall-sidecar.";
const OWNER_LOCK_SUFFIX: &str = ".lock";
pub const KIBITZER_SIDECAR_RETENTION_MS: i64 = 7 * 24 * 60 * 60 * 1000;

pub fn is_valid_encoded_session(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
}

pub fn kibitzer_sidecars_root(recall_dir: &Path) -> PathBuf {
    recall_dir.join(SIDECARS_DIRNAME)
}

pub fn kibitzer_sidecar_owner_lock_path_for(locks_dir: &Path, encoded_session: &str) -> PathBuf {
    locks_dir.join(format!(
        "{OWNER_LOCK_PREFIX}{encoded_session}{OWNER_LOCK_SUFFIX}"
    ))
}

pub type SidecarWarnCallback<'a> = &'a dyn Fn(&str, &str);

pub struct PruneKibitzerSidecarsOptions<'a> {
    pub recall_dir: &'a Path,
    pub locks_dir: &'a Path,
    pub now_ms: &'a dyn Fn() -> i64,
    pub max_age_ms: Option<i64>,
    pub owned: &'a BTreeSet<String>,
    pub warn: Option<SidecarWarnCallback<'a>>,
    pub before_claim_rename: Option<super::policy::BeforeClaimRenameCallback<'a>>,
    pub before_tombstone_delete: Option<&'a dyn Fn(&Path)>,
    pub skip: Option<&'a dyn Fn(&str) -> bool>,
    pub admit: Option<&'a mut dyn FnMut() -> bool>,
    pub done: Option<&'a mut dyn FnMut(&str)>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PruneKibitzerSidecarsResult {
    pub pruned: Vec<String>,
    pub kept: Vec<String>,
    pub kept_reasons: BTreeMap<String, RetentionKeepReason>,
}

fn newest_direct_mtime(dir: &Path) -> Option<i64> {
    let own_meta = std::fs::symlink_metadata(dir).ok()?;
    let own_mtime = own_meta.modified().ok()?;
    let mut newest = i64::try_from(own_mtime.duration_since(UNIX_EPOCH).ok()?.as_millis()).ok()?;
    for name in list_names(dir) {
        let Ok(meta) = std::fs::symlink_metadata(dir.join(name)) else {
            continue;
        };
        let Ok(mtime) = meta.modified() else {
            continue;
        };
        if let Ok(ms) = i64::try_from(mtime.duration_since(UNIX_EPOCH).ok()?.as_millis()) {
            newest = newest.max(ms);
        }
    }
    Some(newest)
}

fn idle_for(dir: &Path, at_ms: i64, max_age_ms: i64) -> bool {
    newest_direct_mtime(dir).is_some_and(|newest| at_ms - newest >= max_age_ms)
}

pub fn claim_kibitzer_sidecar(
    dir: &Path,
    lock_path: &Path,
    at_ms: i64,
    max_age_ms: i64,
    warn: Option<SidecarWarnCallback<'_>>,
    before_claim_rename: Option<super::policy::BeforeClaimRenameCallback<'_>>,
    before_tombstone_delete: Option<&dyn Fn(&Path)>,
) -> Option<RetentionKeepReason> {
    let record = match create_lock_record(KIBITZER_SIDECAR_OWNER_LOCK_PURPOSE, Default::default()) {
        Ok(r) => r,
        Err(err) => {
            if let Some(w) = warn {
                w("kibitzer sidecar sweep lock failed", &err.to_string());
            }
            return Some(RetentionKeepReason::Locked);
        }
    };
    let acquire_opts = AcquireLockOptions {
        wait_timeout_ms: Some(0),
        retry_delay_ms: None,
        cancellation: None,
    };
    match acquire_lock(lock_path, &record, &acquire_opts) {
        Ok(()) => {}
        Err(AcquireLockError::Contention(_)) => return Some(RetentionKeepReason::Locked),
        Err(err) => {
            if let Some(w) = warn {
                w("kibitzer sidecar sweep lock failed", &err.to_string());
            }
            return Some(RetentionKeepReason::Locked);
        }
    }

    let tombstone = dir.with_file_name(format!(
        "{KIBITZER_SIDECAR_TOMBSTONE_PREFIX}{}-{}",
        dir.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("sidecar"),
        random_uuid()
    ));

    let claim_result: Result<(), RetentionKeepReason> = (|| {
        if !idle_for(dir, at_ms, max_age_ms) {
            return Err(RetentionKeepReason::Recent);
        }
        if let Some(before) = before_claim_rename
            && let Err(err) = before(dir, &tombstone)
        {
            if let Some(w) = warn {
                w("kibitzer sidecar sweep claim failed", &err);
            }
            return Err(RetentionKeepReason::ClaimFailed);
        }
        if let Err(err) = std::fs::rename(dir, &tombstone) {
            if err.kind() == std::io::ErrorKind::NotFound {
                return Err(RetentionKeepReason::ChangedDuringRemoval);
            }
            if let Some(w) = warn {
                w("kibitzer sidecar sweep claim failed", &err.to_string());
            }
            return Err(RetentionKeepReason::ClaimFailed);
        }
        Ok(())
    })();

    if let Err(err) = release_lock(lock_path, &record)
        && let Some(w) = warn
    {
        w(
            "kibitzer sidecar sweep lock release failed",
            &err.to_string(),
        );
    }

    if let Err(reason) = claim_result {
        return Some(reason);
    }

    if let Some(before_del) = before_tombstone_delete {
        before_del(&tombstone);
    }
    if let Err(err) = remove_tree(&tombstone)
        && let Some(w) = warn
    {
        w("kibitzer sidecar sweep removal failed", &err.to_string());
    }
    None
}

pub fn prune_kibitzer_sidecars(
    mut options: PruneKibitzerSidecarsOptions<'_>,
) -> PruneKibitzerSidecarsResult {
    let max_age_ms = options.max_age_ms.unwrap_or(KIBITZER_SIDECAR_RETENTION_MS);
    let root = kibitzer_sidecars_root(options.recall_dir);
    let names = match std::fs::read_dir(&root) {
        Ok(entries) => {
            let mut list: Vec<String> = entries
                .filter_map(Result::ok)
                .filter_map(|e| e.file_name().into_string().ok())
                .collect();
            list.sort();
            list
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(err) => {
            if let Some(w) = options.warn {
                w("kibitzer sidecar sweep listing failed", &err.to_string());
            }
            Vec::new()
        }
    };

    let mut pruned = Vec::new();
    let mut kept_reasons = BTreeMap::new();

    for name in names {
        let is_tombstone = name.starts_with(KIBITZER_SIDECAR_TOMBSTONE_PREFIX);
        if !is_tombstone {
            if options.skip.is_some_and(|s| s(&name)) {
                continue;
            }
            if let Some(admit) = options.admit.as_mut()
                && !admit()
            {
                break;
            }
        }
        let path = root.join(&name);
        if is_tombstone {
            if let Err(err) = remove_tree(&path)
                && let Some(w) = options.warn
            {
                w("kibitzer sidecar sweep removal failed", &err.to_string());
            }
        } else if options.owned.contains(&name) {
            kept_reasons.insert(name.clone(), RetentionKeepReason::Owned);
        } else if is_valid_encoded_session(&name)
            && std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_dir())
        {
            let now = (options.now_ms)();
            if !idle_for(&path, now, max_age_ms) {
                kept_reasons.insert(name.clone(), RetentionKeepReason::Recent);
            } else {
                let lock_path = kibitzer_sidecar_owner_lock_path_for(options.locks_dir, &name);
                match claim_kibitzer_sidecar(
                    &path,
                    &lock_path,
                    now,
                    max_age_ms,
                    options.warn,
                    options.before_claim_rename,
                    options.before_tombstone_delete,
                ) {
                    None => pruned.push(name.clone()),
                    Some(reason) => {
                        kept_reasons.insert(name.clone(), reason);
                    }
                }
            }
        }
        if !is_tombstone && let Some(done) = options.done.as_mut() {
            done(&name);
        }
    }

    let kept = kept_reasons.keys().cloned().collect();
    PruneKibitzerSidecarsResult {
        pruned,
        kept,
        kept_reasons,
    }
}
