use std::{collections::{BTreeMap, BTreeSet}, path::{Path, PathBuf}, process::Stdio};
use super::{memory_run_supervisor_ic8_exit_resources::{ExitResources, ExitServer}, memory_run_supervisor_ic8_process_groups::{NativeProcessGroupRuntime, process_group_is_alive, terminate_process_group}, run_artifacts::{RunKind, RunLaunchManifest, write_run_json_atomic}};
pub const IC8_WAIT_MS: u64 = 60_000;
pub const IC8_PLATFORMS: [&str; 2] = ["posix", "win32"];
pub struct Ic8Run { pub root: PathBuf, pub clock: PathBuf, pub exit: ExitServer }
pub struct Ic8Harness {
    roots: Vec<tempfile::TempDir>, pub exit_resources: ExitResources,
    live: BTreeSet<u32>, executable: PathBuf, prefix: Vec<String>,
}
impl Ic8Harness {
    pub fn new(executable: PathBuf, prefix: Vec<String>) -> Self {
        Self { roots: vec![], exit_resources: ExitResources::new(IC8_WAIT_MS), live: BTreeSet::new(), executable, prefix }
    }
    pub async fn make_run(&mut self, mode: &str) -> Result<Ic8Run, String> {
        let root = tempfile::Builder::new().prefix("memory-run-supervisor-ic8-").tempdir().map_err(|error| error.to_string())?;
        let path = root.path().canonicalize().map_err(|error| error.to_string())?;
        let clock = super::supervisor_test_signals::create_test_clock(&path, 1000.0).map_err(|error| error.to_string())?;
        let exit = self.exit_resources.open_server().await?;
        let mut env: BTreeMap<String, String> = std::env::vars().collect();
        env.insert("OMO_MEMORY_SUPERVISOR_EXIT_PORT".into(), exit.port.to_string());
        let args = self.prefix.iter().cloned().chain(["--fixture-supervisor-child".into(), mode.into(), path.to_string_lossy().into_owned()]).collect();
        write_run_json_atomic(&path.join("ledger.json"), &serde_json::json!({"version":1,"runId":"run-ic8","kind":"reflection"}), 0o600).map_err(|error| error.to_string())?;
        write_run_json_atomic(&path.join("launch.json"), &RunLaunchManifest {
            version: 1, run_id: "run-ic8".into(), attempt: 1, next_attempt: None, kind: RunKind::Reflection,
            command: self.executable.to_string_lossy().into_owned(), args, cwd: path.to_string_lossy().into_owned(), env,
            hard_deadline_at: 2000.0, termination_grace_ms: 1000.0, max_output_bytes: 65_536,
            stdout_path: path.join("child-stdout.log").to_string_lossy().into_owned(), stderr_path: path.join("child-stderr.log").to_string_lossy().into_owned(),
        }, 0o600).map_err(|error| error.to_string())?;
        self.roots.push(root);
        Ok(Ic8Run { root: path, clock, exit })
    }
    pub fn launch_supervisor(&mut self, run: &Ic8Run, platform: &str) -> Result<tokio::process::Child, String> {
        let taskkill: Vec<_> = std::iter::once(self.executable.to_string_lossy().into_owned()).chain(self.prefix.clone()).chain(["--fixture-taskkill".into()]).collect();
        let mut command = tokio::process::Command::new(&self.executable);
        command.args(&self.prefix).arg(&run.root).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
            .env("OMO_MEMORY_SUPERVISOR_ALLOW_TEST_SEAMS", "1").env("OMO_MEMORY_SUPERVISOR_PLATFORM", platform)
            .env("OMO_MEMORY_SUPERVISOR_CLOCK_PATH", &run.clock).env("OMO_MEMORY_SUPERVISOR_TASKKILL_RUN_DIR", &run.root)
            .env("OMO_MEMORY_SUPERVISOR_TASKKILL_COMMAND", serde_json::to_string(&taskkill).map_err(|error| error.to_string())?);
        #[cfg(unix)]
        command.process_group(0);
        let child = command.spawn().map_err(|error| error.to_string())?;
        if let Some(pid) = child.id() { self.live.insert(pid); }
        Ok(child)
    }
    pub async fn wait_for_exit(&mut self, child: &mut tokio::process::Child) -> Result<std::process::ExitStatus, String> {
        let pid = child.id();
        let status = self.exit_resources.wait_bounded(child.wait(), IC8_WAIT_MS, "process exit").await?.map_err(|error| error.to_string())?;
        if let Some(pid) = pid { self.live.remove(&pid); }
        Ok(status)
    }
    pub async fn wait_for_path(&self, path: &Path) -> Result<(), String> {
        super::supervisor_test_signals::wait_for_filesystem_state(path.parent().unwrap_or(Path::new(".")), || async { Ok(path.exists().then_some(())) }, IC8_WAIT_MS, &path.to_string_lossy()).await
    }
    pub fn advance_clock(&self, run: &Ic8Run, instant: f64) -> Result<(), String> {
        super::supervisor_test_signals::advance_test_clock(&run.clock, instant).map_err(|error| error.to_string())
    }
    pub fn track_process_group(&mut self, pid: u32) { self.live.insert(pid); }
    pub fn untrack_process_group(&mut self, pid: u32) { self.live.remove(&pid); }
    pub async fn cleanup(&mut self) -> Result<(), String> {
        let runtime = NativeProcessGroupRuntime;
        let mut first_error = None;
        let mut groups = std::mem::take(&mut self.live);
        for root in &self.roots {
            match super::run_artifacts::read_run_json::<serde_json::Value>(&root.path().join("ledger.json")) {
                Ok(ledger) => if self.exit_resources.has_connected_sockets() && let Some(pid) = ledger["childPid"].as_u64().and_then(|pid| u32::try_from(pid).ok()) { groups.insert(pid); },
                Err(error) => { first_error.get_or_insert_with(|| error.to_string()); }
            }
        }
        for &pid in &groups {
            match process_group_is_alive(f64::from(pid), &runtime) {
                Ok(true) => if let Err(error) = terminate_process_group(f64::from(pid), &runtime) { first_error.get_or_insert(error); },
                Ok(false) => {},
                Err(error) => { first_error.get_or_insert(error); }
            }
        }
        match self.exit_resources.cleanup().await {
            Ok(0) => {},
            Ok(forced) => { first_error.get_or_insert_with(|| format!("forced {forced} model socket cleanup(s)")); }
            Err(error) => { first_error.get_or_insert(error); }
        }
        for pid in groups {
            if process_group_is_alive(f64::from(pid),&runtime).unwrap_or(true){
                first_error.get_or_insert_with(||format!("process group {pid} remains live after cleanup"));
            }
        }
        for root in self.roots.drain(..) {
            if let Err(error) = root.close() { first_error.get_or_insert_with(|| error.to_string()); }
        }
        first_error.map_or(Ok(()), Err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn run_manifest_carries_exact_clock_deadline_and_exit_port() {
        let mut harness = Ic8Harness::new("/bin/sh".into(), vec![]);
        let run = harness.make_run("stubborn").await.unwrap();
        let manifest: RunLaunchManifest = super::super::run_artifacts::read_run_json(&run.root.join("launch.json")).unwrap();
        assert_eq!(manifest.hard_deadline_at, 2000.0);
        assert_eq!(manifest.termination_grace_ms, 1000.0);
        assert_eq!(manifest.env["OMO_MEMORY_SUPERVISOR_EXIT_PORT"], run.exit.port.to_string());
        assert_eq!(super::super::supervisor_process_identity::read_injected_clock(&run.clock), 1000.0);
        harness.advance_clock(&run, 3000.0).unwrap();
        assert_eq!(super::super::supervisor_process_identity::read_injected_clock(&run.clock), 3000.0);
        assert!(harness.cleanup().await.is_ok());
        assert!(!run.root.exists());
    }
}
