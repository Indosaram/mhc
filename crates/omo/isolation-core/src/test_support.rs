use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::backend::{
    BackendKind, IsolationBackend, IsolationContext, ProbeResult, Result, StartDetail,
};
use crate::util::mkdtemp_in;

pub struct Fixture {
    pub root: PathBuf,
    pub home_dir: PathBuf,
    pub repo_root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

pub fn fixture() -> Fixture {
    let root = mkdtemp_in(&std::env::temp_dir(), "isolation-core-").expect("temporary directory");
    let home_dir = root.join("home");
    let repo_root = root.join("repo");
    std::fs::create_dir_all(home_dir.join(".omo")).expect("home directory");
    std::fs::create_dir_all(&repo_root).expect("repository directory");
    Fixture {
        root,
        home_dir,
        repo_root,
    }
}

pub type ProbeFn =
    Arc<dyn Fn(&Path, Option<&IsolationContext>) -> Result<ProbeResult> + Send + Sync>;
pub type StartFn =
    Arc<dyn Fn(&Path, &Path, &IsolationContext) -> Result<Option<StartDetail>> + Send + Sync>;
pub type StopFn = Arc<dyn Fn(&Path) -> Result<()> + Send + Sync>;
pub type RelocateFn = Arc<dyn Fn(&Path, &Path) -> Result<()> + Send + Sync>;

pub struct TestBackend {
    pub kind: BackendKind,
    pub clones_tree: bool,
    pub probe_fn: ProbeFn,
    pub start_fn: StartFn,
    pub stop_fn: StopFn,
    pub relocate_fn: Option<RelocateFn>,
}

impl TestBackend {
    pub fn new() -> Self {
        TestBackend {
            kind: BackendKind::Rcopy,
            clones_tree: false,
            probe_fn: Arc::new(|_, _| Ok(ProbeResult::available())),
            start_fn: Arc::new(|_lower, merged, _ctx| {
                std::fs::create_dir_all(merged)?;
                Ok(None)
            }),
            stop_fn: Arc::new(|_merged| Ok(())),
            relocate_fn: None,
        }
    }
}

impl Default for TestBackend {
    fn default() -> Self {
        TestBackend::new()
    }
}

impl IsolationBackend for TestBackend {
    fn kind(&self) -> BackendKind {
        self.kind
    }

    fn clones_tree(&self) -> bool {
        self.clones_tree
    }

    fn probe(&self, repo_root: &Path, ctx: Option<&IsolationContext>) -> Result<ProbeResult> {
        (self.probe_fn)(repo_root, ctx)
    }

    fn start(
        &self,
        lower: &Path,
        merged: &Path,
        ctx: &IsolationContext,
    ) -> Result<Option<StartDetail>> {
        (self.start_fn)(lower, merged, ctx)
    }

    fn stop(&self, merged: &Path) -> Result<()> {
        (self.stop_fn)(merged)
    }

    fn relocate(&self, from: &Path, to: &Path) -> Result<()> {
        match &self.relocate_fn {
            Some(relocate) => relocate(from, to),
            None => {
                std::fs::rename(from, to)?;
                Ok(())
            }
        }
    }
}

pub fn backend() -> Arc<dyn IsolationBackend> {
    Arc::new(TestBackend::new())
}
