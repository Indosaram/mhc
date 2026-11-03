//! Port of senpi `core/session-writer-claims.ts` (senpi#3549): one writer per session file across
//! processes.
//!
//! The terminal builtin's process identity (`process-identity.ts`, `process-start-probe.ts`) lives in
//! maho-ext-terminal, which maho-core cannot depend on; the real probes reuse the schedule runner's
//! own-process identity, `same_boot` tolerance and per-pid start probe instead.
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::schedule::native_probes::read_process_start_ms;
use crate::schedule::runner_lease::{native_identity, native_pid_alive, same_boot};

const WRITERS_DIR: &str = "session-writers";

/// senpi `WriterClaim`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WriterClaim {
    pub generation: u64,
    pub nonce: String,
    /// The machine the writer ran on: its pid, boot and start time mean nothing on another host.
    pub host: String,
    pub pid: u32,
    pub boot_at_ms: u64,
    pub process_started_at_ms: u64,
    pub canonical_path: String,
}

/// senpi `WriterProbes`.
#[async_trait::async_trait]
pub trait WriterProbes: Sync {
    fn pid_alive(&self, pid: u32) -> bool;
    async fn read_process_start_ms(&self, pid: u32) -> Option<u64>;
    fn boot_at_ms(&self) -> u64;
    fn host(&self) -> String;
    /// `(pid, process_started_at_ms)` of this process.
    fn own(&self) -> (u32, u64);
}

/// senpi `SessionWriterBusyError` (`session_writer_busy`) and `SessionWriterClaimError`
/// (`session_writer_claim_invalid`).
#[derive(Debug, thiserror::Error)]
pub enum WriterClaimError {
    #[error(
        "Session is already open for writing by process {}; close it there or open it read-only.",
        .0.pid
    )]
    Busy(Box<WriterClaim>),
    #[error("{0}")]
    Invalid(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl WriterClaimError {
    pub fn code(&self) -> Option<&'static str> {
        match self {
            Self::Busy(_) => Some("session_writer_busy"),
            Self::Invalid(_) => Some("session_writer_claim_invalid"),
            Self::Io(_) => None,
        }
    }
}

/// senpi `realWriterProbes`, over the host process.
pub struct RealWriterProbes {
    boot: u64,
    own: (u32, u64),
}

impl RealWriterProbes {
    pub fn new() -> Result<Self, WriterClaimError> {
        let (boot, start) = native_identity().map_err(WriterClaimError::Invalid)?;
        Ok(Self {
            boot,
            own: (std::process::id(), start),
        })
    }
}

#[async_trait::async_trait]
impl WriterProbes for RealWriterProbes {
    fn pid_alive(&self, pid: u32) -> bool {
        native_pid_alive(pid)
    }
    async fn read_process_start_ms(&self, pid: u32) -> Option<u64> {
        if pid == self.own.0 {
            return Some(self.own.1);
        }
        let boot = self.boot;
        tokio::task::spawn_blocking(move || read_process_start_ms(pid, boot))
            .await
            .ok()
            .and_then(Result::ok)
            .flatten()
    }
    fn boot_at_ms(&self) -> u64 {
        self.boot
    }
    fn host(&self) -> String {
        host_name()
    }
    fn own(&self) -> (u32, u64) {
        self.own
    }
}

/// `os.hostname()`.
fn host_name() -> String {
    for file in ["/proc/sys/kernel/hostname", "/etc/hostname"] {
        if let Ok(name) = std::fs::read_to_string(file)
            && !name.trim().is_empty()
        {
            return name.trim().to_owned();
        }
    }
    if let Ok(name) = std::env::var("COMPUTERNAME")
        && !name.trim().is_empty()
    {
        return name.trim().to_owned();
    }
    std::process::Command::new("hostname")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .unwrap_or_default()
}

/// senpi `writerClaimsDir`.
pub fn writer_claims_dir(session_file: &str, session_id: &str) -> PathBuf {
    let resolved = crate::session_manager::resolve_moved_session_path(session_file);
    Path::new(&resolved)
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(WRITERS_DIR)
        .join(crate::session_sidecar_store::encoded_session_id(session_id))
}

/// Takes the next writer generation, only after proving the current owner can no longer write
/// (senpi#3549).
pub async fn acquire_writer_claim(
    dir: &Path,
    canonical_path: &str,
    probes: &dyn WriterProbes,
) -> Result<WriterClaim, WriterClaimError> {
    tokio::fs::create_dir_all(dir).await?;
    let current = latest_claim(dir).await?;
    if let Some(current) = current.as_ref()
        && !owner_gone(dir, current, probes).await?
    {
        return Err(WriterClaimError::Busy(Box::new(current.clone())));
    }
    let (pid, process_started_at_ms) = probes.own();
    let claim = WriterClaim {
        generation: current.map_or(0, |current| current.generation) + 1,
        nonce: uuid::Uuid::new_v4().to_string(),
        host: probes.host(),
        pid,
        boot_at_ms: probes.boot_at_ms(),
        process_started_at_ms,
        canonical_path: canonical_path.to_owned(),
    };
    let content = serde_json::to_string(&claim).map_err(std::io::Error::other)?;
    match publish_linked(&dir.join(format!("{}.json", claim.generation)), &content).await {
        Ok(()) => Ok(claim),
        Err(WriterClaimError::Io(error)) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            match latest_claim(dir).await? {
                Some(winner) => Err(WriterClaimError::Busy(Box::new(winner))),
                None => Err(error.into()),
            }
        }
        Err(error) => Err(error),
    }
}

/// True only while `claim` is still the newest generation and has not been released.
pub async fn writer_claim_is_current(
    dir: &Path,
    claim: &WriterClaim,
) -> Result<bool, WriterClaimError> {
    let current = latest_claim(dir).await?;
    if current.as_ref().is_none_or(|current| {
        current.generation != claim.generation || current.nonce != claim.nonce
    }) {
        return Ok(false);
    }
    Ok(!released_with(dir, claim).await?)
}

/// Publishes the nonce-matched release marker; call only once nothing can append, consume or
/// dispatch.
pub async fn release_writer_claim(dir: &Path, claim: &WriterClaim) -> Result<(), WriterClaimError> {
    match publish_linked(
        &dir.join(format!("{}.released", claim.generation)),
        &claim.nonce,
    )
    .await
    {
        Err(WriterClaimError::Io(error)) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            Ok(())
        }
        result => result,
    }
}

async fn latest_claim(dir: &Path) -> Result<Option<WriterClaim>, WriterClaimError> {
    let mut generations = Vec::new();
    match tokio::fs::read_dir(dir).await {
        Ok(mut entries) => {
            while let Some(entry) = entries.next_entry().await? {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if let Some(digits) = name.strip_suffix(".json")
                    && !digits.is_empty()
                    && digits.bytes().all(|byte| byte.is_ascii_digit())
                    && let Ok(generation) = digits.parse::<u64>()
                {
                    generations.push(generation);
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    generations.sort_unstable();
    for (index, generation) in generations.iter().enumerate() {
        if *generation != index as u64 + 1 {
            return Err(WriterClaimError::Invalid(format!(
                "Writer claims in {} have a gap before {generation}.",
                dir.display()
            )));
        }
    }
    let Some(last) = generations.last() else {
        return Ok(None);
    };
    let raw = tokio::fs::read_to_string(dir.join(format!("{last}.json"))).await?;
    parse_claim(&raw, dir).map(Some)
}

async fn owner_gone(
    dir: &Path,
    claim: &WriterClaim,
    probes: &dyn WriterProbes,
) -> Result<bool, WriterClaimError> {
    if released_with(dir, claim).await? {
        return Ok(true);
    }
    // A writer on another host (a shared or network directory) cannot be probed from here: never
    // displace it.
    if claim.host != probes.host() {
        return Ok(false);
    }
    if !same_boot(probes.boot_at_ms(), claim.boot_at_ms) {
        return Ok(true);
    }
    if !probes.pid_alive(claim.pid) {
        return Ok(true);
    }
    let Some(started_at_ms) = probes.read_process_start_ms(claim.pid).await else {
        return Ok(false);
    };
    // senpi compares the start instants with BOOT_INSTANT_TOLERANCE_MS, the same_boot tolerance.
    Ok(!same_boot(started_at_ms, claim.process_started_at_ms))
}

async fn released_with(dir: &Path, claim: &WriterClaim) -> Result<bool, WriterClaimError> {
    match tokio::fs::read_to_string(dir.join(format!("{}.released", claim.generation))).await {
        Ok(nonce) => Ok(nonce == claim.nonce),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

async fn publish_linked(path: &Path, content: &str) -> Result<(), WriterClaimError> {
    use tokio::io::AsyncWriteExt;
    let mut temp = path.as_os_str().to_owned();
    temp.push(format!(".{}.tmp", uuid::Uuid::new_v4()));
    let temp = PathBuf::from(temp);
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(&temp).await?;
    let written = async {
        file.write_all(content.as_bytes()).await?;
        file.sync_all().await
    }
    .await;
    drop(file);
    let result = match written {
        Ok(()) => match tokio::fs::hard_link(&temp, path).await {
            Ok(()) => Ok(()),
            Err(error)
                if error.kind() == std::io::ErrorKind::Unsupported
                    || error.kind() == std::io::ErrorKind::PermissionDenied =>
            {
                Err(WriterClaimError::Invalid(format!(
                    "Writer claims need hard links, which {} does not support.",
                    path.parent().unwrap_or_else(|| Path::new(".")).display()
                )))
            }
            Err(error) => Err(error.into()),
        },
        Err(error) => Err(error.into()),
    };
    let _ = tokio::fs::remove_file(&temp).await;
    result
}

fn parse_claim(raw: &str, dir: &Path) -> Result<WriterClaim, WriterClaimError> {
    serde_json::from_str(raw).map_err(|_| {
        WriterClaimError::Invalid(format!("A writer claim in {} is malformed.", dir.display()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, HashSet};

    #[derive(Clone)]
    struct World {
        alive: HashSet<u32>,
        starts: HashMap<u32, u64>,
        boot: u64,
    }

    fn world() -> World {
        World {
            alive: HashSet::from([100, 200]),
            starts: HashMap::from([(100, 1_000), (200, 2_000)]),
            boot: 50,
        }
    }

    struct Probes {
        world: World,
        pid: u32,
        host: &'static str,
    }

    fn probes_for(world: &World, pid: u32) -> Probes {
        probes_on(world, pid, "host-a")
    }

    fn probes_on(world: &World, pid: u32, host: &'static str) -> Probes {
        Probes {
            world: world.clone(),
            pid,
            host,
        }
    }

    #[async_trait::async_trait]
    impl WriterProbes for Probes {
        fn pid_alive(&self, pid: u32) -> bool {
            self.world.alive.contains(&pid)
        }
        async fn read_process_start_ms(&self, pid: u32) -> Option<u64> {
            self.world.starts.get(&pid).copied()
        }
        fn boot_at_ms(&self) -> u64 {
            self.world.boot
        }
        fn host(&self) -> String {
            self.host.to_owned()
        }
        fn own(&self) -> (u32, u64) {
            (
                self.pid,
                self.world.starts.get(&self.pid).copied().unwrap_or(0),
            )
        }
    }

    fn busy(result: Result<WriterClaim, WriterClaimError>) -> bool {
        matches!(result, Err(WriterClaimError::Busy(_)))
    }

    #[tokio::test]
    async fn refuses_a_second_writer_before_recovery_or_delivery_reservation() {
        let dir = tempfile::tempdir().expect("dir");
        let w = world();
        acquire_writer_claim(dir.path(), "/s.jsonl", &probes_for(&w, 100))
            .await
            .expect("first");
        let second = acquire_writer_claim(dir.path(), "/s.jsonl", &probes_for(&w, 200)).await;
        assert_eq!(
            second.as_ref().err().and_then(WriterClaimError::code),
            Some("session_writer_busy")
        );
    }

    #[tokio::test]
    async fn allows_one_takeover_only_after_the_prior_writer_cannot_dispatch() {
        let dir = tempfile::tempdir().expect("dir");
        let mut w = world();
        let first = acquire_writer_claim(dir.path(), "/s.jsonl", &probes_for(&w, 100))
            .await
            .expect("first");
        w.alive.remove(&100);
        let second = acquire_writer_claim(dir.path(), "/s.jsonl", &probes_for(&w, 200))
            .await
            .expect("second");
        assert_eq!(second.generation, 2);
        assert!(
            !writer_claim_is_current(dir.path(), &first)
                .await
                .expect("first")
        );
        assert!(
            writer_claim_is_current(dir.path(), &second)
                .await
                .expect("second")
        );
    }

    #[tokio::test]
    async fn hands_over_after_the_owner_publishes_its_release_marker() {
        let dir = tempfile::tempdir().expect("dir");
        let w = world();
        let first = acquire_writer_claim(dir.path(), "/s.jsonl", &probes_for(&w, 100))
            .await
            .expect("first");
        release_writer_claim(dir.path(), &first)
            .await
            .expect("release");
        release_writer_claim(dir.path(), &first)
            .await
            .expect("release twice");
        assert!(
            !writer_claim_is_current(dir.path(), &first)
                .await
                .expect("current")
        );
        let second = acquire_writer_claim(dir.path(), "/s.jsonl", &probes_for(&w, 200))
            .await
            .expect("second");
        assert_eq!(second.generation, 2);
    }

    #[tokio::test]
    async fn never_displaces_a_live_owner_whose_start_time_cannot_be_read() {
        let dir = tempfile::tempdir().expect("dir");
        let mut w = world();
        acquire_writer_claim(dir.path(), "/s.jsonl", &probes_for(&w, 100))
            .await
            .expect("first");
        w.starts.remove(&100);
        assert!(busy(
            acquire_writer_claim(dir.path(), "/s.jsonl", &probes_for(&w, 200)).await
        ));
    }

    #[tokio::test]
    async fn takes_over_from_a_reused_pid_and_from_an_older_boot() {
        let reused = tempfile::tempdir().expect("reused");
        let mut w = world();
        acquire_writer_claim(reused.path(), "/s.jsonl", &probes_for(&w, 100))
            .await
            .expect("first");
        w.starts.insert(100, 9_000_000);
        let claim = acquire_writer_claim(reused.path(), "/s.jsonl", &probes_for(&w, 200))
            .await
            .expect("reused pid");
        assert_eq!(claim.generation, 2);

        let rebooted = tempfile::tempdir().expect("rebooted");
        let mut w2 = world();
        acquire_writer_claim(rebooted.path(), "/s.jsonl", &probes_for(&w2, 100))
            .await
            .expect("first");
        w2.boot = 50 + 10_000_000;
        let claim = acquire_writer_claim(rebooted.path(), "/s.jsonl", &probes_for(&w2, 200))
            .await
            .expect("older boot");
        assert_eq!(claim.generation, 2);
    }

    #[tokio::test]
    async fn gives_two_takeover_contenders_one_owner() {
        let dir = tempfile::tempdir().expect("dir");
        let mut w = world();
        w.alive.insert(300);
        w.starts.insert(300, 3_000);
        acquire_writer_claim(dir.path(), "/s.jsonl", &probes_for(&w, 100))
            .await
            .expect("first");
        w.alive.remove(&100);
        let (a, b) = (probes_for(&w, 200), probes_for(&w, 300));
        let (left, right) = tokio::join!(
            acquire_writer_claim(dir.path(), "/s.jsonl", &a),
            acquire_writer_claim(dir.path(), "/s.jsonl", &b)
        );
        let outcomes = [left, right];
        assert_eq!(outcomes.iter().filter(|outcome| outcome.is_ok()).count(), 1);
        assert!(
            outcomes
                .into_iter()
                .any(|outcome| matches!(outcome, Err(WriterClaimError::Busy(_))))
        );
    }

    #[tokio::test]
    async fn never_takes_over_a_writer_on_another_host() {
        let dir = tempfile::tempdir().expect("dir");
        let mut w = world();
        acquire_writer_claim(dir.path(), "/s.jsonl", &probes_on(&w, 100, "host-b"))
            .await
            .expect("first");
        w.alive.remove(&100);
        w.boot = 9_999_999;
        assert!(busy(
            acquire_writer_claim(dir.path(), "/s.jsonl", &probes_on(&w, 200, "host-a")).await
        ));
    }

    #[tokio::test]
    async fn refuses_a_chain_with_a_missing_generation_instead_of_guessing() {
        let dir = tempfile::tempdir().expect("dir");
        let dead_owner = serde_json::json!({
            "generation": 2,
            "nonce": "n",
            "host": "host-a",
            "pid": 999,
            "bootAtMs": 50,
            "processStartedAtMs": 1,
            "canonicalPath": "/s.jsonl",
        });
        std::fs::write(dir.path().join("2.json"), dead_owner.to_string()).expect("claim");
        let result = acquire_writer_claim(dir.path(), "/s.jsonl", &probes_for(&world(), 200)).await;
        assert!(matches!(result, Err(WriterClaimError::Invalid(_))));
    }

    #[cfg(unix)]
    #[test]
    fn the_claims_dir_sits_beside_the_session_file_under_the_encoded_id() {
        let dir = writer_claims_dir("/sessions/project/s.jsonl", "a/b");
        assert_eq!(dir, Path::new("/sessions/project/session-writers/a%2Fb"));
    }
}
