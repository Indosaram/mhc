use std::path::Path;
use std::sync::Arc;

use crate::backend::{
    BackendKind, IsolationBackend, IsolationContext, IsolationError, ProbeResult, Result,
};
use crate::backend_marker::mark_started;
use crate::backends::runtime::{checked, existing_parent, BackendRuntime, SystemRuntime};
use crate::git::command::exists;
use crate::util::dirname;

pub struct BtrfsBackend {
    io: Arc<dyn BackendRuntime>,
}

impl Default for BtrfsBackend {
    fn default() -> Self {
        BtrfsBackend {
            io: Arc::new(SystemRuntime),
        }
    }
}

impl BtrfsBackend {
    pub fn new(io: Arc<dyn BackendRuntime>) -> Self {
        BtrfsBackend { io }
    }

    fn probe_inner(&self, lower: &Path, ctx: Option<&IsolationContext>) -> Result<ProbeResult> {
        if self.io.platform() != "linux" || !self.io.which("btrfs") {
            return Ok(ProbeResult::unavailable(
                "btrfs requires Linux and btrfs on PATH",
            ));
        }
        if ctx.map(|ctx| ctx.cross_device).unwrap_or(false) {
            return Ok(ProbeResult::unavailable("btrfs requires the same filesystem"));
        }
        let result = self
            .io
            .run(&argv(&["btrfs", "subvolume", "show", &lower.to_string_lossy()]))?;
        if result.code != 0
            && is_not_a_subvolume(&result.stderr)
        {
            return Ok(ProbeResult::unavailable(result.stderr));
        }
        if result.code != 0 && !is_inconclusive(&result.stderr) {
            // An I/O or similar operational failure says nothing about btrfs capability.
            return Err(IsolationError::other(format!(
                "btrfs subvolume show {} failed ({}): {}",
                lower.display(),
                result.code,
                result.stderr
            )));
        }
        if let Some(ctx) = ctx {
            // A subvolume reports its own st_dev, so device numbers cannot decide
            // "same filesystem" on the very filesystem this backend exists for.
            // The mount's filesystem UUID can: both paths must live on the same
            // btrfs, or the snapshot's cross-filesystem refusal stands.
            let target = existing_parent(&ctx.base_dir)?;
            let fstype = self.io.run(&argv(&[
                "findmnt",
                "-no",
                "FSTYPE",
                "--target",
                &target.to_string_lossy(),
            ]))?;
            if fstype.code != 0 || fstype.stdout.trim() != "btrfs" {
                return Ok(ProbeResult::unavailable("btrfs requires the same filesystem"));
            }
            let lower_id = self.io.run(&argv(&[
                "findmnt",
                "-no",
                "UUID",
                "--target",
                &lower.to_string_lossy(),
            ]))?;
            let target_id = self.io.run(&argv(&[
                "findmnt",
                "-no",
                "UUID",
                "--target",
                &target.to_string_lossy(),
            ]))?;
            if lower_id.code != 0
                || target_id.code != 0
                || lower_id.stdout.trim() != target_id.stdout.trim()
            {
                return Ok(ProbeResult::unavailable("btrfs requires the same filesystem"));
            }
        }
        Ok(ProbeResult::available())
    }
}

fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|part| (*part).to_string()).collect()
}

fn is_not_a_subvolume(stderr: &str) -> bool {
    let lower = stderr.to_lowercase();
    [
        "not a btrfs",
        "not a subvolume",
        "cannot find",
        "no such",
        "unknown subvolume",
        "not a directory",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn is_inconclusive(stderr: &str) -> bool {
    let lower = stderr.to_lowercase();
    [
        "operation not permitted",
        "permission denied",
        "could not search b-tree",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn is_capability_failure(stderr: &str) -> bool {
    let lower = stderr.to_lowercase();
    [
        "not a subvolume",
        "not a btrfs",
        "operation not permitted",
        "permission denied",
        "operation not supported",
        "cross-device",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

impl IsolationBackend for BtrfsBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::Btrfs
    }

    fn clones_tree(&self) -> bool {
        true
    }

    fn probe(&self, lower: &Path, ctx: Option<&IsolationContext>) -> Result<ProbeResult> {
        self.probe_inner(lower, ctx)
    }

    fn start(
        &self,
        lower: &Path,
        merged: &Path,
        ctx: &IsolationContext,
    ) -> Result<Option<crate::backend::StartDetail>> {
        std::fs::create_dir_all(dirname(merged))?;
        if ctx.cross_device {
            return Err(IsolationError::unavailable("btrfs requires the same device"));
        }
        let probe = self.probe_inner(lower, None)?;
        if !probe.available {
            return Err(IsolationError::unavailable(
                probe.reason.unwrap_or_default(),
            ));
        }
        let result = self.io.run(&argv(&[
            "btrfs",
            "subvolume",
            "snapshot",
            &lower.to_string_lossy(),
            &merged.to_string_lossy(),
        ]))?;
        if result.code != 0 {
            if is_capability_failure(&result.stderr) {
                return Err(IsolationError::unavailable(result.stderr));
            }
            return Err(IsolationError::other(format!(
                "btrfs snapshot failed: {}",
                result.stderr
            )));
        }
        mark_started(&ctx.base_dir, self.kind(), &[])?;
        Ok(None)
    }

    fn stop(&self, merged: &Path) -> Result<()> {
        if !exists(merged)? {
            return Ok(());
        }
        checked(
            self.io.as_ref(),
            &argv(&["btrfs", "subvolume", "delete", &merged.to_string_lossy()]),
        )?;
        remove_dir_all_force(merged)?;
        Ok(())
    }
}

pub fn remove_dir_all_force(path: &Path) -> Result<()> {
    match std::fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}
