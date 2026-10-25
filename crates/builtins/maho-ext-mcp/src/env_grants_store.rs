//! Shared declaration-scoped environment grants. No environment values are persisted.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GrantLabel {
    pub project: String,
    pub skill: String,
    pub server: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Grant {
    pub label: GrantLabel,
    pub names: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GrantSnapshot {
    pub version: u8,
    pub generation: u64,
    pub grants: BTreeMap<String, Grant>,
}
impl Default for GrantSnapshot {
    fn default() -> Self {
        Self {
            version: 1,
            generation: 0,
            grants: BTreeMap::new(),
        }
    }
}
#[derive(Clone, Debug)]
pub struct GrantObservation {
    pub snapshot: GrantSnapshot,
    pub conservative: bool,
}
pub fn resolve_grants_store_path(config_dir: Option<&Path>) -> PathBuf {
    config_dir
        .map(Path::to_path_buf)
        .unwrap_or_else(|| {
            std::env::var_os("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config")
                })
                .join("omo")
        })
        .join("mcp-env-grants.json")
}
fn valid_digest(digest: &str) -> bool {
    digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
fn valid_name(name: &str) -> bool {
    !name.is_empty() && !name.contains(['=', '\0'])
}
fn parse(value: &[u8]) -> Option<GrantSnapshot> {
    let mut snapshot: GrantSnapshot = serde_json::from_slice(value).ok()?;
    if snapshot.version != 1
        || snapshot.generation > 9_007_199_254_740_991
        || snapshot.grants.iter().any(|(digest, grant)| {
            !valid_digest(digest) || grant.names.iter().any(|name| !valid_name(name))
        })
    {
        return None;
    }
    for grant in snapshot.grants.values_mut() {
        grant.names.sort();
        grant.names.dedup();
    }
    Some(snapshot)
}
pub fn read(path: &Path) -> GrantSnapshot {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| parse(&bytes))
        .unwrap_or_default()
}
fn canonical(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let sorted: BTreeMap<_, _> = map
                .iter()
                .map(|(key, value)| (key.clone(), canonical(value)))
                .collect();
            serde_json::to_value(sorted).expect("JSON map")
        }
        Value::Array(values) => Value::Array(values.iter().map(canonical).collect()),
        _ => value.clone(),
    }
}
pub fn declaration_digest(identity: &Value) -> String {
    format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&canonical(identity)).expect("JSON identity"))
    )
}
fn update(
    path: &Path,
    mutate: impl FnOnce(&mut BTreeMap<String, Grant>),
) -> std::io::Result<GrantSnapshot> {
    let directory = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(directory)?;
    // OS-owned exclusive lock is released on process death and cannot be reclaimed while held.
    let lock_path = path.with_extension("json.lock");
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path)?;
    lock.lock()?;
    let mut next = read(path);
    next.generation = next
        .generation
        .checked_add(1)
        .filter(|generation| *generation <= 9_007_199_254_740_991)
        .ok_or_else(|| std::io::Error::other("MCP grant generation exhausted"))?;
    mutate(&mut next.grants);
    let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        temporary
            .as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    serde_json::to_writer(&mut temporary, &next)?;
    temporary.write_all(b"\n")?;
    temporary.as_file().sync_all()?;
    temporary.persist(path).map_err(|error| error.error)?;
    #[cfg(unix)]
    File::open(directory)?.sync_all()?;
    lock.unlock()?;
    Ok(next)
}
pub fn add_grant(
    path: &Path,
    digest: &str,
    label: GrantLabel,
    names: &[String],
) -> std::io::Result<GrantSnapshot> {
    if !valid_digest(digest) || names.iter().any(|name| !valid_name(name)) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Invalid MCP grant identity or variable name",
        ));
    }
    update(path, |grants| {
        let mut all = grants
            .get(digest)
            .map_or_else(Vec::new, |grant| grant.names.clone());
        all.extend_from_slice(names);
        all.sort();
        all.dedup();
        grants.insert(digest.into(), Grant { label, names: all });
    })
}
pub fn revoke(
    path: &Path,
    digest: &str,
    names: Option<&[String]>,
) -> std::io::Result<GrantSnapshot> {
    update(path, |grants| {
        if let Some(grant) = grants.get_mut(digest) {
            grant
                .names
                .retain(|name| names.is_some_and(|names| !names.contains(name)));
            if grant.names.is_empty() {
                grants.remove(digest);
            }
        }
    })
}
pub fn list(path: &Path) -> Vec<(String, Grant)> {
    read(path).grants.into_iter().collect()
}
/// Latest-snapshot reconciliation. Counter resets, missed generations and invalid reads revoke conservatively.
pub struct GrantObserver {
    previous: Option<(GrantSnapshot, Option<std::time::SystemTime>)>,
}
impl Default for GrantObserver {
    fn default() -> Self {
        Self { previous: None }
    }
}
impl GrantObserver {
    pub fn reconcile(&mut self, path: &Path) -> Option<GrantObservation> {
        let bytes = std::fs::read(path).ok();
        let parsed = bytes.as_deref().and_then(parse);
        let valid = parsed.is_some();
        let snapshot = parsed.unwrap_or_default();
        let modified = std::fs::metadata(path)
            .ok()
            .and_then(|metadata| metadata.modified().ok());
        let conservative = self.previous.as_ref().is_some_and(|(old, stamp)| {
            !valid
                || snapshot.generation > old.generation.saturating_add(1)
                || snapshot.generation < old.generation
                || (modified != *stamp && snapshot.generation <= old.generation)
        });
        let unchanged = self.previous.as_ref().is_some_and(|(old, stamp)| {
            *stamp == modified
                && declaration_digest(&serde_json::to_value(old).expect("snapshot"))
                    == declaration_digest(&serde_json::to_value(&snapshot).expect("snapshot"))
        });
        self.previous = Some((snapshot.clone(), modified));
        if unchanged {
            None
        } else {
            Some(GrantObservation {
                snapshot,
                conservative,
            })
        }
    }
}
pub fn observe(
    path: PathBuf,
    listener: std::sync::Arc<dyn Fn(GrantObservation) + Send + Sync>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut observer = GrantObserver::default();
        let mut timer = tokio::time::interval(std::time::Duration::from_secs(5));
        loop {
            timer.tick().await;
            if let Some(observation) = observer.reconcile(&path) {
                listener(observation);
            }
        }
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn digest_sorts_objects_not_arrays() {
        assert_eq!(
            declaration_digest(&serde_json::json!({"b":2,"a":1})),
            declaration_digest(&serde_json::json!({"a":1,"b":2}))
        );
        assert_ne!(
            declaration_digest(&serde_json::json!([1, 2])),
            declaration_digest(&serde_json::json!([2, 1]))
        );
    }
    #[test]
    fn grants_merge_revoke_and_fail_closed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("grants.json");
        let digest = declaration_digest(&serde_json::json!({"raw":{}}));
        let label = GrantLabel {
            project: "p".into(),
            skill: "s".into(),
            server: "m".into(),
        };
        add_grant(&path, &digest, label.clone(), &["Z".into(), "A".into()]).unwrap();
        let next = add_grant(&path, &digest, label, &["A".into(), "B".into()]).unwrap();
        assert_eq!(next.generation, 2);
        assert_eq!(next.grants[&digest].names, ["A", "B", "Z"]);
        revoke(&path, &digest, Some(&["B".into()])).unwrap();
        assert_eq!(read(&path).grants[&digest].names, ["A", "Z"]);
        revoke(&path, &digest, None).unwrap();
        assert!(read(&path).grants.is_empty());
        std::fs::write(&path, "invalid").unwrap();
        assert!(read(&path).grants.is_empty());
    }
}
