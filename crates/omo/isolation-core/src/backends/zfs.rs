use std::path::Path;
use std::sync::Arc;

use crate::backend::{
    BackendKind, IsolationBackend, IsolationContext, IsolationError, ProbeResult, Result,
    StartDetail, BACKEND_FILE,
};
use crate::backend_marker::mark_started;
use crate::backends::btrfs::remove_dir_all_force;
use crate::backends::runtime::{checked, BackendRuntime, SystemRuntime};
use crate::util::{dirname, resolve_path};

pub struct ZfsBackend {
    io: Arc<dyn BackendRuntime>,
}

impl Default for ZfsBackend {
    fn default() -> Self {
        ZfsBackend {
            io: Arc::new(SystemRuntime),
        }
    }
}

impl ZfsBackend {
    pub fn new(io: Arc<dyn BackendRuntime>) -> Self {
        ZfsBackend { io }
    }

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|part| (*part).to_string()).collect()
    }

    fn dataset(&self, lower: &Path) -> Result<Option<String>> {
        if !self.io.which("zfs") {
            return Ok(None);
        }
        let result = self.io.run(&Self::argv(&["zfs", "list", "-H", "-o", "name,mountpoint"]))?;
        if result.code != 0 {
            if is_missing_pool(&result.stderr) {
                return Ok(None);
            }
            return Err(IsolationError::other(format!(
                "zfs list failed ({}): {}",
                result.code,
                result.stderr
            )));
        }
        let target = resolve_path(lower);
        for line in result.stdout.split('\n') {
            let mut parts = line.split('\t');
            let Some(name) = parts.next() else { continue };
            let Some(mount) = parts.next() else { continue };
            if mount.starts_with('/') && resolve_path(Path::new(mount)) == target {
                return Ok(Some(name.to_string()));
            }
        }
        Ok(None)
    }

    fn marker(&self, base: &Path) -> Result<Option<ZfsMarker>> {
        let path = base.join(BACKEND_FILE);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let marker: ZfsMarker = serde_json::from_str(&text)
            .map_err(|error| IsolationError::other(error.to_string()))?;
        if marker.dataset.is_none() && marker.snapshot.is_none() {
            return Ok(None);
        }
        let snapshot = marker.snapshot.clone().unwrap_or_default();
        let (source, suffix) = match snapshot.split_once("@omo-") {
            Some((source, suffix)) => (source.to_string(), suffix.to_string()),
            None => (String::new(), String::new()),
        };
        let expected = format!("{source}/omo-{suffix}");
        if marker.backend.as_deref() != Some("zfs")
            || source.is_empty()
            || suffix.is_empty()
            || !is_valid_id(&suffix)
            || marker.dataset.as_deref() != Some(expected.as_str())
        {
            return Err(IsolationError::other("Invalid ZFS isolation marker"));
        }
        Ok(Some(marker))
    }

    /// Capability failures (a dataset not delegated to us) fall through; the rest propagate.
    fn capable(&self, argv: &[String]) -> Result<()> {
        match checked(self.io.as_ref(), argv) {
            Ok(_) => Ok(()),
            Err(error) => {
                let message = error.to_string();
                let lower = message.to_lowercase();
                if ["permission denied", "dataset does not exist", "not delegated"]
                    .iter()
                    .any(|needle| lower.contains(needle))
                    || (lower.contains("pool ") && lower.contains(" does not exist"))
                {
                    return Err(IsolationError::unavailable(message));
                }
                Err(error)
            }
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct ZfsMarker {
    backend: Option<String>,
    dataset: Option<String>,
    snapshot: Option<String>,
    cloned: Option<bool>,
}

fn is_missing_pool(stderr: &str) -> bool {
    let lower = stderr.to_lowercase();
    [
        "no datasets",
        "dataset does not exist",
        "permission denied",
        "failed to initialize",
        "no pools available",
        "cannot open",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
        || (lower.contains("pool ") && lower.contains(" does not exist"))
}

fn is_valid_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-')
}

fn write_marker(base: &Path, marker: &ZfsMarker) -> Result<()> {
    let text = serde_json::to_string(marker)
        .map_err(|error| IsolationError::other(error.to_string()))?;
    std::fs::write(base.join(BACKEND_FILE), text)?;
    Ok(())
}

impl IsolationBackend for ZfsBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::Zfs
    }

    fn clones_tree(&self) -> bool {
        true
    }

    fn probe(&self, lower: &Path, _ctx: Option<&IsolationContext>) -> Result<ProbeResult> {
        // ZFS delegation is a Linux capability; say so up front on other platforms
        // instead of failing the dataset parse against foreign path forms.
        if self.io.platform() != "linux" {
            return Ok(ProbeResult::unavailable(
                "zfs requires Linux and a delegated ZFS dataset root",
            ));
        }
        Ok(ProbeResult {
            available: self.dataset(lower)?.is_some(),
            reason: Some("source must be a delegated ZFS dataset root".to_string()),
        })
    }

    fn start(
        &self,
        lower: &Path,
        merged: &Path,
        ctx: &IsolationContext,
    ) -> Result<Option<StartDetail>> {
        let Some(source) = self.dataset(lower)? else {
            return Err(IsolationError::unavailable(
                "source is not a ZFS dataset root",
            ));
        };
        if !is_valid_id(&ctx.id) {
            return Err(IsolationError::other("Invalid ZFS isolation id"));
        }
        let snapshot = format!("{source}@omo-{}", ctx.id);
        let dataset = format!("{source}/omo-{}", ctx.id);
        std::fs::create_dir_all(&ctx.base_dir)?;
        self.capable(&Self::argv(&["zfs", "snapshot", &snapshot]))?;
        // Persist the snapshot before clone so a crash or clone failure can be reclaimed.
        write_marker(
            &ctx.base_dir,
            &ZfsMarker {
                backend: Some("zfs".to_string()),
                dataset: Some(dataset.clone()),
                snapshot: Some(snapshot.clone()),
                cloned: Some(false),
            },
        )?;
        self.capable(&Self::argv(&[
            "zfs",
            "clone",
            "-o",
            &format!("mountpoint={}", merged.display()),
            &snapshot,
            &dataset,
        ]))?;
        write_marker(
            &ctx.base_dir,
            &ZfsMarker {
                backend: Some("zfs".to_string()),
                dataset: Some(dataset),
                snapshot: Some(snapshot),
                cloned: Some(true),
            },
        )?;
        mark_started(&ctx.base_dir, self.kind(), &[])?;
        Ok(None)
    }

    fn relocate(&self, from: &Path, to: &Path) -> Result<()> {
        let marker = self.marker(from)?;
        let Some(marker) = marker else {
            return Err(IsolationError::other("Missing ZFS clone marker"));
        };
        if marker.cloned != Some(true) {
            return Err(IsolationError::other("Missing ZFS clone marker"));
        }
        let dataset = marker.dataset.clone().unwrap_or_default();
        std::fs::rename(from, to)?;
        let result = self.capable(&Self::argv(&[
            "zfs",
            "set",
            &format!("mountpoint={}", to.join("m").display()),
            &dataset,
        ]));
        if let Err(error) = result {
            std::fs::rename(to, from)?;
            return Err(error);
        }
        Ok(())
    }

    fn stop(&self, merged: &Path) -> Result<()> {
        let base = dirname(merged);
        let marker = self.marker(&base)?;
        if let Some(mut marker) = marker {
            if marker.cloned == Some(true) {
                let dataset = marker.dataset.clone().unwrap_or_default();
                self.capable(&Self::argv(&["zfs", "destroy", "-r", &dataset]))?;
                marker.cloned = Some(false);
                write_marker(&base, &marker)?;
            }
            let snapshot = marker.snapshot.clone().unwrap_or_default();
            self.capable(&Self::argv(&["zfs", "destroy", &snapshot]))?;
            write_marker(
                &base,
                &ZfsMarker {
                    backend: Some("zfs".to_string()),
                    dataset: None,
                    snapshot: None,
                    cloned: None,
                },
            )?;
        }
        remove_dir_all_force(merged)
    }
}
