use super::env_grants_store::{self as shared, Grant, GrantSnapshot, Observation};
pub use shared::{GrantLabel, GrantObservation};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
#[derive(Clone)]
pub struct EnvGrantSnapshot {
    pub snapshot: GrantSnapshot,
    pub revision: String,
}
pub trait GrantStore: Send + Sync {
    fn read(&self) -> EnvGrantSnapshot;
    fn add(&self, digest: &str, grant: Grant) -> super::Result<()>;
    fn revoke(&self, digest: &str) -> super::Result<()>;
    fn watch(&self, listener: Box<dyn Fn(GrantObservation) + Send>) -> Option<Observation>;
    fn list(&self) -> std::collections::BTreeMap<String, Grant> {
        self.read().snapshot.grants
    }
    fn digest(&self) -> String {
        shared::declaration_digest(
            &serde_json::to_value(self.read().snapshot).expect("snapshot serializes"),
        )
    }
}
pub struct FileGrantStore {
    pub path: PathBuf,
}
impl GrantStore for FileGrantStore {
    fn read(&self) -> EnvGrantSnapshot {
        EnvGrantSnapshot {
            snapshot: shared::read(&self.path),
            revision: std::fs::metadata(&self.path)
                .map(|m| shared::revision(&m))
                .unwrap_or_else(|_| "missing".into()),
        }
    }
    fn add(&self, digest: &str, grant: Grant) -> super::Result<()> {
        shared::add_grant(&self.path, digest, grant.label, &grant.names).map(|_| ())
    }
    fn revoke(&self, digest: &str) -> super::Result<()> {
        shared::revoke(&self.path, digest, None).map(|_| ())
    }
    fn watch(&self, listener: Box<dyn Fn(GrantObservation) + Send>) -> Option<Observation> {
        Some(shared::observe(self.path.clone(), listener))
    }
}
pub fn create_file_grant_store(config_dir: Option<&Path>) -> Arc<dyn GrantStore> {
    Arc::new(FileGrantStore {
        path: shared::resolve_grants_store_path(config_dir),
    })
}
