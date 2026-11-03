use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};
pub const SESSION_PATH_RETRY_AFTER_MS: u64 = 2000;
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SessionPathOwner {
    pub instance_id: String,
    /// The public socket of the host that made the claim (senpi `host_socket`); absent on a stdio host's claim.
    #[serde(rename = "host_socket", skip_serializing_if = "Option::is_none")]
    pub host_socket: Option<String>,
    pub pid: u32,
    pub process_start_time: Option<String>,
    pub session_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attached: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current: Option<bool>,
}
pub struct SessionPathClaim {
    pub file: PathBuf,
    pub owner: SessionPathOwner,
}
pub fn reservation_file(dir: &Path, session_path: &str) -> PathBuf {
    dir.join(format!(
        "{}.json",
        &format!("{:x}", Sha256::digest(session_path.as_bytes()))[..16]
    ))
}
#[cfg(target_os = "linux")]
pub fn read_process_start_time(pid: u32) -> Option<String> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    stat.rsplit_once(") ")?
        .1
        .split_whitespace()
        .nth(19)
        .map(str::to_owned)
}
/// darwin start identity from libproc `proc_bsdinfo` (arg 1 = zombies too), read in process: the
/// pinned source spawns `ps -o lstart=`, which is the probe-child shape #1507 removed elsewhere.
#[cfg(target_os = "macos")]
pub fn read_process_start_time(pid: u32) -> Option<String> {
    let info =
        libproc::proc_pid::pidinfo::<libproc::bsd_info::BSDInfo>(i32::try_from(pid).ok()?, 1)
            .ok()?;
    Some(format!(
        "{}.{:06}",
        info.pbi_start_tvsec, info.pbi_start_tvusec
    ))
}
#[cfg(not(unix))]
pub fn read_process_start_time(pid: u32) -> Option<String> {
    if pid == std::process::id() {
        static OWN_START: std::sync::OnceLock<String> = std::sync::OnceLock::new();
        Some(OWN_START.get_or_init(|| {
            std::process::Command::new("powershell")
                .args(["-NoProfile", "-NonInteractive", "-Command",
                    &format!("(Get-CimInstance Win32_Process -Filter 'ProcessId={pid}').CreationDate.ToFileTimeUtc().ToString()")])
                .output()
                .ok()
                .and_then(|output| {
                    let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
                    if s.chars().all(|c| c.is_ascii_digit()) && !s.is_empty() { Some(s) } else { None }
                })
                .unwrap_or_else(|| {
                    format!("{:?}", std::time::SystemTime::UNIX_EPOCH.elapsed().unwrap_or_default().as_millis())
                })
        }).clone())
    } else {
        std::process::Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command",
                &format!("(Get-CimInstance Win32_Process -Filter 'ProcessId={pid}').CreationDate.ToFileTimeUtc().ToString()")])
            .output()
            .ok()
            .and_then(|output| {
                let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if s.chars().all(|c| c.is_ascii_digit()) && !s.is_empty() { Some(s) } else { None }
            })
    }
}
#[cfg(unix)]
pub fn process_is_live(pid: u32) -> bool {
    let Some(pid) = i32::try_from(pid)
        .ok()
        .and_then(rustix::process::Pid::from_raw)
    else {
        return false;
    };
    match rustix::process::test_kill_process(pid) {
        Ok(()) => true,
        Err(error) => error == rustix::io::Errno::PERM,
    }
}
#[cfg(not(unix))]
pub fn process_is_live(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    if pid == std::process::id() {
        return true;
    }
    std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}")])
        .output()
        .map(|output| {
            let stdout = String::from_utf8_lossy(&output.stdout);
            stdout.contains(&pid.to_string())
                && !stdout.contains("No tasks are running")
                && !stdout.contains("INFO:")
        })
        .unwrap_or(false)
}
/// Whether `pid` is a process still RUNNING: live, and not an exited-but-unreaped zombie (state Z), which
/// makes no more writes (senpi `processIsRunning`, senpi#4119). Reads the kernel state in process instead of
/// spawning `ps -o stat=`; an unreadable state on a live pid answers running, as the pinned source does.
pub fn process_is_running(pid: u32) -> bool {
    if !process_is_live(pid) {
        return false;
    }
    !process_is_zombie(pid)
}
#[cfg(target_os = "linux")]
fn process_is_zombie(pid: u32) -> bool {
    fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|stat| {
            stat.rsplit_once(") ")
                .and_then(|(_, rest)| rest.chars().next())
        })
        .is_some_and(|state| state == 'Z')
}
#[cfg(target_os = "macos")]
fn process_is_zombie(pid: u32) -> bool {
    /// `SZOMB` from `<sys/proc.h>`.
    const SZOMB: u32 = 5;
    i32::try_from(pid)
        .ok()
        .and_then(|pid| libproc::proc_pid::pidinfo::<libproc::bsd_info::BSDInfo>(pid, 1).ok())
        .is_some_and(|info| info.pbi_status == SZOMB)
}
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn process_is_zombie(_pid: u32) -> bool {
    false
}
/// `Proven`: the pid runs and its start time matches the claim. `Gone`: the pid is dead, or runs with a
/// different start time. `Unproven`: the pid runs but no start time was recorded or none can be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimIdentity {
    Proven,
    Gone,
    Unproven,
}
pub fn claim_owner_identity(owner: &SessionPathOwner) -> ClaimIdentity {
    // An exited, unreaped owner (state Z) makes no more writes: the claim is gone (senpi#4119).
    if !process_is_running(owner.pid) {
        return ClaimIdentity::Gone;
    }
    let Some(recorded) = owner.process_start_time.as_deref().and_then(start_time_ms) else {
        return ClaimIdentity::Unproven;
    };
    // Recorded identities may carry another form than this probe reads: compare the parsed instant, never the
    // raw text (senpi `sameProcessStartMs`).
    match read_process_start_time(owner.pid)
        .as_deref()
        .and_then(start_time_ms)
    {
        None => ClaimIdentity::Unproven,
        Some(observed) => {
            if maho_tool_child::group_ownership::same_process_start_ms(
                Some(recorded),
                Some(observed),
            ) {
                ClaimIdentity::Proven
            } else {
                ClaimIdentity::Gone
            }
        }
    }
}
/// senpi `processStartTimeMs`: a recorded or observed start identity as milliseconds since the epoch.
fn start_time_ms(identity: &str) -> Option<f64> {
    maho_tool_child::group_ownership::process_start_time_ms(identity)
}
pub fn claim_owner_is_live(owner: &SessionPathOwner) -> bool {
    claim_owner_identity(owner) != ClaimIdentity::Gone
}
fn read_owner(file: &Path) -> Option<SessionPathOwner> {
    let value: serde_json::Value = serde_json::from_str(&fs::read_to_string(file).ok()?).ok()?;
    Some(SessionPathOwner {
        instance_id: value["instanceId"].as_str()?.into(),
        host_socket: value["host_socket"].as_str().map(str::to_owned),
        pid: u32::try_from(value["pid"].as_u64()?).ok()?,
        session_path: value["sessionPath"].as_str()?.into(),
        process_start_time: value["processStartTime"].as_str().map(str::to_owned),
        attached: value["attached"].as_bool(),
        current: None,
    })
}
pub fn read_session_path_claims(dir: &Path) -> Vec<SessionPathClaim> {
    let Ok(entries) = fs::read_dir(dir) else {
        return vec![];
    };
    entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().ends_with(".json"))
        .filter_map(|entry| {
            read_owner(&entry.path()).map(|owner| SessionPathClaim {
                file: entry.path(),
                owner,
            })
        })
        .collect()
}
/// How a claim another process published stands against this host.
#[derive(Debug, Clone, PartialEq)]
pub enum StandingVerdict {
    Stands(SessionPathOwner),
    Released,
    /// The owner's identity is unproven, it is not the current generation and its heartbeat stopped. It may still
    /// be alive (a frozen loop also stops the heartbeat), so a takeover fences it out of the session file first.
    PresumedDone,
}
/// Whether the owner's generation left a heartbeat older than a stalled host could explain. A claim file is
/// untrusted input: an instance id that is not a plain directory name names no generation.
fn heartbeat_stopped(paths: &crate::host_daemon_paths::HostDaemonPaths, instance_id: &str) -> bool {
    if instance_id.is_empty()
        || instance_id == "."
        || instance_id == ".."
        || instance_id.contains('/')
        || instance_id.contains('\\')
    {
        return false;
    }
    crate::host_stalled_evidence::generation_heartbeat_stopped(
        &crate::host_daemon_paths::generation_paths(paths, instance_id),
        crate::host_stalled_evidence::now_ms(),
    )
}
pub fn standing_owner(
    mut owner: SessionPathOwner,
    paths: &crate::host_daemon_paths::HostDaemonPaths,
) -> StandingVerdict {
    let identity = claim_owner_identity(&owner);
    if identity == ClaimIdentity::Gone {
        return StandingVerdict::Released;
    }
    let pointer = crate::host_daemon_state::read_file_or_undefined(&paths.pointer_file)
        .ok()
        .flatten();
    let parsed = crate::host_daemon_state::parse_json(pointer.as_deref());
    let current = parsed
        .as_ref()
        .and_then(|record| record.get("instance_id"))
        .and_then(serde_json::Value::as_str)
        == Some(owner.instance_id.as_str());
    if !current
        && identity == ClaimIdentity::Unproven
        && heartbeat_stopped(paths, &owner.instance_id)
    {
        return StandingVerdict::PresumedDone;
    }
    if current || owner.attached != Some(false) {
        owner.current = Some(current);
        StandingVerdict::Stands(owner)
    } else {
        StandingVerdict::Released
    }
}
/// Gives the session file a new write-fence token (`<session file>.fence`), so a writer that opened it before this
/// call and re-reads the token refuses its next write. Same file and format as senpi's `advanceSessionFence`.
fn advance_session_fence(session_file: &str) -> std::io::Result<()> {
    let file = format!("{session_file}.fence");
    let temporary = format!(
        "{file}.{}.{}.tmp",
        std::process::id(),
        &uuid::Uuid::new_v4().to_string()[..8]
    );
    let result = (|| {
        #[cfg(unix)]
        use std::os::unix::fs::OpenOptionsExt;
        let mut options = fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        options.mode(0o600);
        std::io::Write::write_all(
            &mut options.open(&temporary)?,
            format!("{}\n", uuid::Uuid::new_v4()).as_bytes(),
        )?;
        fs::rename(&temporary, &file)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}
pub struct SessionPathReservations {
    paths: crate::host_daemon_paths::HostDaemonPaths,
    instance_id: String,
    public_socket: Option<String>,
    pid: u32,
    start_time: Option<String>,
    held: HashSet<String>,
}
impl SessionPathReservations {
    pub fn new(daemon_dir: &Path, instance_id: String, pid: u32) -> Self {
        Self {
            paths: crate::host_daemon_paths::host_daemon_directory_paths(daemon_dir),
            instance_id,
            public_socket: None,
            pid,
            start_time: read_process_start_time(pid),
            held: HashSet::new(),
        }
    }
    /// The public socket every claim names (senpi `publicSocket`).
    pub fn with_public_socket(mut self, public_socket: Option<String>) -> Self {
        self.public_socket = public_socket;
        self
    }
    /// Live processes registered to this daemon - claim owners and every generation's host child - never inferred
    /// from a holder's own labels (senpi `holderPids`). This process is never one. With `observed_starts`, only a
    /// pid already probed by the caller is classified, against the start it observed.
    pub fn holder_pids(
        &self,
        observed_starts: Option<&std::collections::HashMap<u32, Option<f64>>>,
    ) -> Vec<u32> {
        let mut candidates: Vec<(u32, Option<String>)> =
            read_session_path_claims(&self.paths.reservations_dir)
                .into_iter()
                .map(|claim| (claim.owner.pid, claim.owner.process_start_time))
                .collect();
        for entry in fs::read_dir(&self.paths.generations_dir)
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        {
            let record = crate::host_daemon_state::parse_json(
                crate::host_daemon_state::read_file_or_undefined(
                    &entry.path().join("host-child.pid"),
                )
                .ok()
                .flatten()
                .as_deref(),
            );
            let Some(record) = record else { continue };
            let Some(pid) = record
                .get("pid")
                .and_then(serde_json::Value::as_u64)
                .and_then(|pid| u32::try_from(pid).ok())
            else {
                continue;
            };
            candidates.push((
                pid,
                record
                    .get("processStartTime")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned),
            ));
        }
        let mut by_pid: std::collections::BTreeMap<u32, Vec<Option<String>>> = Default::default();
        for (pid, start) in candidates {
            if pid != std::process::id() {
                by_pid.entry(pid).or_default().push(start);
            }
        }
        by_pid
            .into_iter()
            .filter(|(pid, starts)| {
                if !process_is_live(*pid) {
                    return false;
                }
                let observed = match observed_starts {
                    // Only foreign-looking holders need family classification; their probes were already done.
                    Some(observed) => match observed.get(pid) {
                        None => return false,
                        Some(start) => *start,
                    },
                    None => read_process_start_time(*pid)
                        .as_deref()
                        .and_then(start_time_ms),
                };
                observed.is_some()
                    && starts.iter().any(|start| {
                        start
                            .as_deref()
                            .and_then(start_time_ms)
                            .is_some_and(|recorded| {
                                maho_tool_child::group_ownership::same_process_start_ms(
                                    Some(recorded),
                                    observed,
                                )
                            })
                    })
            })
            .map(|(pid, _)| pid)
            .collect()
    }
    fn publish(&self, session_path: &str, attached: bool) -> std::io::Result<()> {
        #[cfg(unix)]
        use std::os::unix::fs::DirBuilderExt;
        let owner = SessionPathOwner {
            instance_id: self.instance_id.clone(),
            host_socket: self.public_socket.clone(),
            pid: self.pid,
            process_start_time: self.start_time.clone(),
            session_path: session_path.into(),
            attached: Some(attached),
            current: None,
        };
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        builder.mode(0o700);
        builder.create(&self.paths.reservations_dir)?;
        let file = reservation_file(&self.paths.reservations_dir, session_path);
        let staging = file.with_file_name(format!(
            "{}.{}.tmp",
            file.file_name().unwrap_or_default().to_string_lossy(),
            self.pid
        ));
        crate::host_daemon_state::write_state_file(&staging, &owner)
            .map_err(|error| error.source)?;
        fs::rename(staging, file)
    }
    pub fn claim(
        &mut self,
        session_path: &str,
        attached: bool,
        mut report: impl FnMut(String),
    ) -> Option<SessionPathOwner> {
        if let Some(existing) = read_owner(&reservation_file(
            &self.paths.reservations_dir,
            session_path,
        )) && existing.pid != self.pid
        {
            match standing_owner(existing.clone(), &self.paths) {
                StandingVerdict::Stands(standing) => return Some(standing),
                StandingVerdict::Released => {}
                StandingVerdict::PresumedDone => {
                    if let Err(error) = advance_session_fence(session_path) {
                        report(format!(
                            "session file {session_path} could not be fenced for a takeover ({error})"
                        ));
                        return Some(SessionPathOwner {
                            current: Some(false),
                            ..existing
                        });
                    }
                }
            }
        }
        match self.publish(session_path, attached) {
            Ok(()) => {
                self.held.insert(session_path.into());
            }
            Err(error) => report(format!(
                "session path reservation for {session_path} could not be written ({error})"
            )),
        }
        None
    }
    pub fn release(&mut self, session_path: &str, mut report: impl FnMut(String)) {
        if !self.held.remove(session_path) {
            return;
        }
        if let Err(error) =
            fs::remove_file(reservation_file(&self.paths.reservations_dir, session_path))
            && error.kind() != std::io::ErrorKind::NotFound
        {
            report(format!(
                "session path reservation for {session_path} could not be removed ({error})"
            ));
        }
    }
    pub fn set_attached(&self, session_path: &str, attached: bool, mut report: impl FnMut(String)) {
        if self.held.contains(session_path)
            && let Err(error) = self.publish(session_path, attached)
        {
            report(format!(
                "session path reservation for {session_path} could not be updated ({error})"
            ));
        }
    }
}
/// A path-reservation set with its failure reporter bound once (senpi's factory shape).
pub struct BoundPathReservations {
    inner: SessionPathReservations,
    report: Box<dyn FnMut(String) + Send>,
}
impl BoundPathReservations {
    pub fn claim(&mut self, session_path: &str, attached: bool) -> Option<SessionPathOwner> {
        let report = &mut self.report;
        self.inner.claim(session_path, attached, report)
    }
    pub fn release(&mut self, session_path: &str) {
        let report = &mut self.report;
        self.inner.release(session_path, report);
    }
    pub fn set_attached(&mut self, session_path: &str, attached: bool) {
        let report = &mut self.report;
        self.inner.set_attached(session_path, attached, report);
    }
    /// senpi `holderPids`.
    pub fn holder_pids(
        &self,
        observed_starts: Option<&std::collections::HashMap<u32, Option<f64>>>,
    ) -> Vec<u32> {
        self.inner.holder_pids(observed_starts)
    }
    /// senpi `publicSocket`: every claim names the public socket of the host that made it.
    pub fn with_public_socket(mut self, public_socket: Option<String>) -> Self {
        self.inner = self.inner.with_public_socket(public_socket);
        self
    }
}
/// This endpoint's daemon directory: the claims live in it, and the pointer they are read
/// against lives there too (senpi `createSessionPathReservations`).
pub fn create_session_path_reservations(
    daemon_dir: &Path,
    instance_id: String,
    pid: Option<u32>,
    on_failure: impl FnMut(String) + Send + 'static,
) -> BoundPathReservations {
    BoundPathReservations {
        inner: SessionPathReservations::new(
            daemon_dir,
            instance_id,
            pid.unwrap_or_else(std::process::id),
        ),
        report: Box::new(on_failure),
    }
}
/// The claims a shared host publishes, or nothing when it serves no socket and no daemon
/// directory was told through the environment (senpi `createEndpointReservations`).
pub fn create_endpoint_reservations(
    agent_dir: &Path,
    socket: Option<&str>,
    instance_id: String,
    on_failure: impl FnMut(String) + Send + 'static,
) -> Option<BoundPathReservations> {
    let told = std::env::var(crate::host_daemon_paths::HOST_DAEMON_DIR_ENV).ok();
    let dir = endpoint_reservations_dir(agent_dir, socket, told.as_deref())?;
    Some(create_session_path_reservations(
        &dir,
        instance_id,
        None,
        on_failure,
    ))
}
/// The daemon directory endpoint claims live in, or none when nothing tells this process where to
/// publish them (senpi `createEndpointReservations`): a told directory wins, else the socket's own
/// directory, else there is no endpoint to share. Split from the environment read so the decision is
/// deterministic in tests instead of depending on the ambient `SENPI_RPC_HOST_DAEMON_DIR`.
fn endpoint_reservations_dir(
    agent_dir: &Path,
    socket: Option<&str>,
    told: Option<&str>,
) -> Option<PathBuf> {
    match told.filter(|value| !value.trim().is_empty()) {
        Some(dir) => Some(PathBuf::from(dir)),
        None => socket.map(|socket| {
            crate::host_daemon_paths::create_host_daemon_paths(socket, agent_dir).dir
        }),
    }
}
/// A test child left as an exited-but-unreaped zombie. Dropping it reaps the child, so an early test
/// failure (an unwrap or assert before the end) still leaves no zombie behind.
#[cfg(all(test, target_os = "linux"))]
pub(crate) struct ZombieChild(std::process::Child);
#[cfg(all(test, target_os = "linux"))]
impl ZombieChild {
    /// Spawns `/usr/bin/true` and waits for its exit without reaping it.
    pub(crate) fn spawn() -> Self {
        let child = Self(
            std::process::Command::new("/usr/bin/true")
                .spawn()
                .expect("spawn zombie test child"),
        );
        rustix::process::waitid(
            rustix::process::WaitId::Pid(rustix::process::Pid::from_child(&child.0)),
            rustix::process::WaitIdOptions::EXITED | rustix::process::WaitIdOptions::NOWAIT,
        )
        .expect("waitid for zombie test child");
        child
    }
    pub(crate) fn id(&self) -> u32 {
        self.0.id()
    }
}
#[cfg(all(test, target_os = "linux"))]
impl Drop for ZombieChild {
    fn drop(&mut self) {
        let _ = self.0.wait();
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bound_reservations_claim_republish_and_release_through_the_reporter() {
        let temp = tempfile::tempdir().unwrap();
        let failures = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorder = failures.clone();
        let mut reservations =
            create_session_path_reservations(temp.path(), "one".into(), None, move |message| {
                recorder
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(message)
            });
        assert!(reservations.claim("/session", true).is_none());
        let dir = temp.path().join("reservations");
        assert_eq!(read_session_path_claims(&dir).len(), 1);
        reservations.set_attached("/session", false);
        assert_eq!(
            read_session_path_claims(&dir)[0].owner.attached,
            Some(false)
        );
        reservations.release("/session");
        assert!(read_session_path_claims(&dir).is_empty());
        assert!(
            failures
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_empty()
        );
    }
    #[test]
    fn endpoint_reservations_need_a_socket_or_a_daemon_directory() {
        let temp = tempfile::tempdir().unwrap();
        assert!(endpoint_reservations_dir(temp.path(), None, None).is_none());
        assert!(endpoint_reservations_dir(temp.path(), None, Some("   ")).is_none());
        assert!(endpoint_reservations_dir(temp.path(), Some("/tmp/rpc.sock"), None).is_some());
        assert_eq!(
            endpoint_reservations_dir(temp.path(), None, Some("/told")),
            Some(PathBuf::from("/told"))
        );
        let bound =
            create_endpoint_reservations(temp.path(), Some("/tmp/rpc.sock"), "one".into(), |_| {});
        assert!(bound.is_some());
    }
    #[test]
    fn claims_republish_and_release() {
        let temp = tempfile::tempdir().unwrap();
        let mut reservations =
            SessionPathReservations::new(temp.path(), "one".into(), std::process::id());
        assert!(
            reservations
                .claim("/session", true, |error| panic!("{error}"))
                .is_none()
        );
        let dir = temp.path().join("reservations");
        assert_eq!(read_session_path_claims(&dir).len(), 1);
        reservations.set_attached("/session", false, |error| panic!("{error}"));
        assert_eq!(
            read_session_path_claims(&dir)[0].owner.attached,
            Some(false)
        );
        reservations.release("/session", |error| panic!("{error}"));
        assert!(read_session_path_claims(&dir).is_empty());
    }
    #[test]
    fn superseded_detached_claim_is_reclaimable_but_current_stands() {
        let temp = tempfile::tempdir().unwrap();
        let paths = crate::host_daemon_paths::host_daemon_directory_paths(temp.path());
        let pointer = paths.pointer_file.clone();
        let owner = SessionPathOwner {
            instance_id: "one".into(),
            host_socket: None,
            pid: std::process::id(),
            process_start_time: read_process_start_time(std::process::id()),
            session_path: "/session".into(),
            attached: Some(false),
            current: None,
        };
        assert_eq!(
            standing_owner(owner.clone(), &paths),
            StandingVerdict::Released
        );
        fs::write(&pointer, r#"{"instance_id":"one"}"#).unwrap();
        assert!(
            matches!(standing_owner(owner.clone(),&paths),StandingVerdict::Stands(stands) if stands.current==Some(true))
        );
        let mut recycled = owner;
        // A parseable start that is not this process's: the pid was recycled.
        recycled.process_start_time = Some("1".into());
        assert!(!claim_owner_is_live(&recycled));
        assert_eq!(claim_owner_identity(&recycled), ClaimIdentity::Gone);
        let mut unrecorded = recycled;
        unrecorded.process_start_time = None;
        assert_eq!(claim_owner_identity(&unrecorded), ClaimIdentity::Unproven);
    }
    /// host-28 / senpi#4119: an exited, unreaped (state Z) claim owner makes no more writes, so its claim is gone
    /// even though signal 0 still reaches the pid.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_zombie_claim_owner_is_gone() {
        let child = ZombieChild::spawn();
        let pid = child.id();
        assert!(process_is_live(pid));
        assert!(!process_is_running(pid));
        let owner = SessionPathOwner {
            instance_id: "z".into(),
            host_socket: None,
            pid,
            process_start_time: read_process_start_time(pid),
            session_path: "/session".into(),
            attached: Some(true),
            current: None,
        };
        assert_eq!(claim_owner_identity(&owner), ClaimIdentity::Gone);
        assert!(!claim_owner_is_live(&owner));
        let temp = tempfile::tempdir().unwrap();
        let paths = crate::host_daemon_paths::host_daemon_directory_paths(temp.path());
        fs::write(&paths.pointer_file, r#"{"instance_id":"z"}"#).unwrap();
        assert_eq!(standing_owner(owner, &paths), StandingVerdict::Released);
    }
    /// 1893-generation-records-and-claims / issue-2951-worker-claims: claims name their host socket, and the holder
    /// set is the live claim owners plus every generation's host child, never this process and never a recycled pid.
    #[cfg(target_os = "linux")]
    #[test]
    fn claims_name_the_public_socket_and_holders_are_proven_registered_processes() {
        let temp = tempfile::tempdir().unwrap();
        let mut sleeper = std::process::Command::new("sleep")
            .arg("300")
            .spawn()
            .unwrap();
        let pid = sleeper.id();
        let start = read_process_start_time(pid);
        let mut reservations =
            create_session_path_reservations(temp.path(), "one".into(), Some(pid), |error| {
                panic!("{error}")
            })
            .with_public_socket(Some("/public.sock".into()));
        assert!(reservations.claim("/session", true).is_none());
        let claims = read_session_path_claims(&temp.path().join("reservations"));
        assert_eq!(claims[0].owner.host_socket.as_deref(), Some("/public.sock"));
        let generation = temp.path().join("generations/g1");
        fs::create_dir_all(&generation).unwrap();
        fs::write(
            generation.join("host-child.pid"),
            serde_json::json!({"pid":std::process::id(),"processStartTime":read_process_start_time(std::process::id())}).to_string(),
        )
        .unwrap();
        assert_eq!(reservations.holder_pids(None), vec![pid]);
        let observed =
            std::collections::HashMap::from([(pid, start.as_deref().and_then(start_time_ms))]);
        assert_eq!(reservations.holder_pids(Some(&observed)), vec![pid]);
        let recycled = std::collections::HashMap::from([(pid, Some(0.0))]);
        assert!(reservations.holder_pids(Some(&recycled)).is_empty());
        assert!(
            reservations
                .holder_pids(Some(&Default::default()))
                .is_empty()
        );
        sleeper.kill().unwrap();
        sleeper.wait().unwrap();
        assert!(reservations.holder_pids(None).is_empty());
    }
    #[test]
    fn unproven_stale_heartbeat_claim_is_presumed_done_and_fenced_on_takeover() {
        let temp = tempfile::tempdir().unwrap();
        let paths = crate::host_daemon_paths::host_daemon_directory_paths(temp.path());
        let generation = crate::host_daemon_paths::generation_paths(&paths, "old");
        fs::create_dir_all(&generation.dir).unwrap();
        fs::write(
            &generation.alive_file,
            serde_json::json!({"at":"2000-01-01T00:00:00Z"}).to_string(),
        )
        .unwrap();
        fs::write(&paths.pointer_file, r#"{"instance_id":"other"}"#).unwrap();
        let owner = SessionPathOwner {
            instance_id: "old".into(),
            host_socket: None,
            pid: std::process::id(),
            process_start_time: None,
            session_path: temp.path().join("s.jsonl").to_string_lossy().into_owned(),
            attached: Some(true),
            current: None,
        };
        assert_eq!(
            standing_owner(owner.clone(), &paths),
            StandingVerdict::PresumedDone
        );
        assert_eq!(
            standing_owner(
                SessionPathOwner {
                    instance_id: "../old".into(),
                    ..owner.clone()
                },
                &paths
            ),
            StandingVerdict::Stands(SessionPathOwner {
                instance_id: "../old".into(),
                current: Some(false),
                ..owner.clone()
            })
        );
        advance_session_fence(&owner.session_path).unwrap();
        assert!(
            !fs::read_to_string(format!("{}.fence", owner.session_path))
                .unwrap()
                .trim()
                .is_empty()
        );
    }
}
