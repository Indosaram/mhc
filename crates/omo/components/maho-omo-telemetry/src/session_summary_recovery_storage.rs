//! Atomic recovery hand-over and PID-reuse-aware owner probes.
use crate::{
    session_summary_storage::*,
    session_summary_store::{SummaryStore, parse_window_record},
};
use serde_json::Value;
use std::{
    fs, io,
    path::{Path, PathBuf},
};

pub struct Claimed {
    pub record: Option<Value>,
    pub claim_path: PathBuf,
}
impl SummaryStore {
    pub(crate) fn is_owner_alive(&self, owner: &Value) -> io::Result<bool> {
        if owner == &self.owner {
            return Ok(true);
        }
        let Some(pid) = owner["pid"].as_i64() else {
            return Ok(false);
        };
        if !senpi_task::dag::store::default_is_process_alive(pid) {
            return Ok(false);
        }
        let path = self.dir.join("owners").join(format!("{pid}.json"));
        let Some(entry) = read_json(&path)? else {
            return Ok(false);
        };
        if entry["bootId"] != owner["bootId"] {
            return Ok(false);
        }
        Ok(
            match (entry["startTime"].as_str(), read_process_start_time(pid)) {
                (Some(recorded), Some(current)) => recorded == current,
                _ => true,
            },
        )
    }
    pub(crate) fn claim_window(&self, key: &str) -> io::Result<Option<Claimed>> {
        claim_file(&self.path(key), &self.owner, parse_window_record)
    }
    pub(crate) fn claim_pending(&self, uuid: &str) -> io::Result<Option<Claimed>> {
        claim_file(&self.pending_path(uuid), &self.owner, parse_pending_record)
    }
    pub(crate) fn has_pending(&self, uuid: &str) -> io::Result<bool> {
        Ok(list_names(&self.dir.join("pending"))?.iter().any(|name| {
            name == &format!("{uuid}.json") || name.starts_with(&format!("{uuid}.json.claimed-"))
        }))
    }
    pub(crate) fn sweep_leftovers(&self) -> io::Result<()> {
        for dir in [&self.dir, &self.dir.join("pending")] {
            for name in list_names(dir)? {
                let claim = name.rsplit_once(".claimed-");
                let temp = name
                    .strip_suffix(".tmp")
                    .and_then(|name| name.rsplit_once('.'));
                let Some((base, suffix)) = claim.or(temp) else {
                    continue;
                };
                let Some((pid, boot)) = suffix.split_once('-') else {
                    continue;
                };
                let Ok(pid) = pid.parse::<i64>() else {
                    continue;
                };
                let owner = serde_json::json!({"pid":pid,"bootId":boot});
                if self.is_owner_alive(&owner)? {
                    continue;
                }
                let path = dir.join(&name);
                if claim.is_some() {
                    // Link instead of replacing: a newer writer's base always wins.
                    match fs::hard_link(&path, dir.join(base)) {
                        Ok(()) => {}
                        Err(e)
                            if matches!(
                                e.kind(),
                                io::ErrorKind::AlreadyExists | io::ErrorKind::NotFound
                            ) => {}
                        Err(e) => return Err(e),
                    }
                }
                remove_file(&path)?;
            }
        }
        Ok(())
    }
}
fn claim_file(
    path: &Path,
    owner: &Value,
    parse: fn(Value) -> Option<Value>,
) -> io::Result<Option<Claimed>> {
    let claim_path = PathBuf::from(format!(
        "{}.claimed-{}-{}",
        path.display(),
        owner["pid"],
        owner["bootId"].as_str().unwrap_or_default()
    ));
    match fs::rename(path, &claim_path) {
        Ok(()) => Ok(Some(Claimed {
            record: read_json(&claim_path)?.and_then(parse),
            claim_path,
        })),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}
