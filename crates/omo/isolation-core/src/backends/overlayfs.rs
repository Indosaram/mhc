use std::path::Path;
use std::sync::Arc;

use crate::backend::{
    BackendKind, IsolationBackend, IsolationContext, IsolationError, ProbeResult, Result,
    StartDetail, BACKEND_FILE,
};
use crate::backend_marker::mark_started;
use crate::backends::btrfs::remove_dir_all_force;
use crate::backends::runtime::{checked, BackendRuntime, SystemRuntime};
use crate::util::dirname;

pub struct OverlayfsBackend {
    io: Arc<dyn BackendRuntime>,
}

impl Default for OverlayfsBackend {
    fn default() -> Self {
        OverlayfsBackend {
            io: Arc::new(SystemRuntime),
        }
    }
}

impl OverlayfsBackend {
    pub fn new(io: Arc<dyn BackendRuntime>) -> Self {
        OverlayfsBackend { io }
    }

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|part| (*part).to_string()).collect()
    }

    fn probe_inner(&self) -> Result<ProbeResult> {
        let available = self.io.platform() == "linux"
            && self.io.which("fuse-overlayfs")
            && self.io.which("fusermount3")
            && self.io.accessible(Path::new("/dev/fuse"))?;
        Ok(ProbeResult {
            available,
            reason: Some(
                "fuse-overlayfs requires Linux, fusermount3 and accessible /dev/fuse".to_string(),
            ),
        })
    }

    fn mount(&self, lower: &Path, base: &Path) -> Result<()> {
        let lower_text = lower.to_string_lossy().into_owned();
        let base_text = base.to_string_lossy().into_owned();
        // Commas/colons are option separators even without a shell; reject rather than mis-mount.
        if [',', ':', '\n', '\\']
            .iter()
            .any(|separator| lower_text.contains(*separator) || base_text.contains(*separator))
        {
            return Err(IsolationError::unavailable(
                "overlay paths contain unsupported mount-option separators",
            ));
        }
        for name in ["upper", "work", "m"] {
            std::fs::create_dir_all(base.join(name))?;
        }
        let result = checked(
            self.io.as_ref(),
            &Self::argv(&[
                "fuse-overlayfs",
                "-o",
                &format!(
                    "lowerdir={lower_text},upperdir={},workdir={}",
                    base.join("upper").display(),
                    base.join("work").display()
                ),
                &base.join("m").to_string_lossy(),
            ]),
        );
        if let Err(error) = result {
            let message = error.to_string();
            let lower = message.to_lowercase();
            // A missing or unusable FUSE device is a capability gap other backends can fill.
            if [
                "/dev/fuse",
                "fusermount",
                "permission denied",
                "operation not permitted",
                "fuse device",
            ]
            .iter()
            .any(|needle| lower.contains(needle))
            {
                return Err(IsolationError::unavailable(message));
            }
            return Err(error);
        }
        self.io.wait_mounted(&base.join("m"))
    }

    fn unmount(&self, merged: &Path) -> Result<()> {
        if !self.io.mounted(merged)? {
            return Ok(());
        }
        let mut message = String::new();
        for _ in 0..3 {
            let result = self.io.run(&Self::argv(&[
                "fusermount3",
                "-u",
                &merged.to_string_lossy(),
            ]))?;
            if result.code == 0 {
                return Ok(());
            }
            message = result.stderr;
        }
        Err(IsolationError::other(format!("fusermount3 failed: {message}")))
    }
}

impl IsolationBackend for OverlayfsBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::Overlayfs
    }

    fn clones_tree(&self) -> bool {
        true
    }

    fn probe(&self, _lower: &Path, _ctx: Option<&IsolationContext>) -> Result<ProbeResult> {
        self.probe_inner()
    }

    fn start(
        &self,
        lower: &Path,
        _merged: &Path,
        ctx: &IsolationContext,
    ) -> Result<Option<StartDetail>> {
        let probe = self.probe_inner()?;
        if !probe.available {
            return Err(IsolationError::unavailable(
                probe.reason.unwrap_or_default(),
            ));
        }
        self.mount(lower, &ctx.base_dir)?;
        mark_started(
            &ctx.base_dir,
            self.kind(),
            &[("lower", lower.to_string_lossy().into_owned())],
        )?;
        Ok(None)
    }

    fn relocate(&self, from: &Path, to: &Path) -> Result<()> {
        let text = std::fs::read_to_string(from.join(BACKEND_FILE))?;
        let marker: serde_json::Value = serde_json::from_str(&text)
            .map_err(|error| IsolationError::other(error.to_string()))?;
        let lower = marker
            .get("lower")
            .and_then(|lower| lower.as_str())
            .ok_or_else(|| IsolationError::other("Invalid overlay marker"))?;
        if marker.get("backend").and_then(|backend| backend.as_str()) != Some("overlayfs") {
            return Err(IsolationError::other("Invalid overlay marker"));
        }
        let lower = Path::new(lower).to_path_buf();
        // No pathname changes before successful unmount; a busy mount fails closed.
        self.unmount(&from.join("m"))?;
        std::fs::rename(from, to)?;
        match self.mount(&lower, to) {
            Ok(()) => Ok(()),
            Err(error) => {
                // Restore the original parent and mount so ensure can tear down at its known path.
                let _ = self.unmount(&to.join("m"));
                std::fs::rename(to, from)?;
                let _ = self.mount(&lower, from);
                Err(error)
            }
        }
    }

    fn stop(&self, merged: &Path) -> Result<()> {
        self.unmount(merged)?;
        remove_dir_all_force(&dirname(merged))
    }
}
