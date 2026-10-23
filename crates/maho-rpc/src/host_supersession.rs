use crate::socket_ownership::EndpointOwnership;
/// Handle for the supersession watcher task; dropping or stopping it ends the watch.
pub struct SupersessionWatch {
    task: tokio::task::JoinHandle<()>,
}
impl SupersessionWatch {
    pub fn stop(self) {
        self.task.abort();
    }
}
/// Runs `on_lost` once this endpoint stops being served by the entry this generation bound
/// (senpi `watchForSupersession`). Without an identity, supersession can never be proven and is
/// never claimed, so the watch is inert.
pub fn watch_for_supersession(
    path: String,
    identity: Option<crate::socket_ownership::SocketFileIdentity>,
    settled: impl Fn() -> bool + Send + Sync + 'static,
    on_lost: impl FnOnce(EndpointLoss) + Send + 'static,
) -> SupersessionWatch {
    let task = tokio::spawn(async move {
        if let Some(loss) = wait_for_supersession(&path, identity, settled).await {
            on_lost(loss);
        }
    });
    SupersessionWatch { task }
}
pub const SUPERSESSION_POLL_MS: u64 = 1000;
/// Consecutive agreeing pointer + endpoint observations before an identity-less endpoint counts as replaced.
pub const REPLACED_CONFIRMATIONS: u32 = 3;
/// Bound on one identity probe of the public endpoint; an unanswered probe is no evidence.
pub const IDENTITY_PROBE_TIMEOUT_MS: u64 = 2000;
/// The daemon pointer file and this generation's instance id: the evidence used when no file identity exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SupersessionPointer {
    pub file: std::path::PathBuf,
    pub instance_id: String,
}
/// Pointer-based watch for an endpoint with no file identity (a win32 named pipe); never reports `Absent`.
#[derive(Default)]
pub struct PointerSupersessionWatcher {
    agreeing_polls: u32,
    lost: bool,
}
impl PointerSupersessionWatcher {
    /// `replaced` is true only when the pointer names another instance AND the endpoint answered as that instance.
    pub fn observe(&mut self, replaced: bool, settled: bool) -> Option<EndpointLoss> {
        if settled || self.lost {
            return None;
        }
        self.agreeing_polls = if replaced { self.agreeing_polls + 1 } else { 0 };
        if self.agreeing_polls < REPLACED_CONFIRMATIONS {
            return None;
        }
        self.lost = true;
        Some(EndpointLoss::Replaced)
    }
}
/// The instance id the pointer file names, if it parses and names one.
pub fn read_pointer_instance_id(file: &std::path::Path) -> Option<String> {
    let parsed: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(file).ok()?).ok()?;
    parsed
        .get("instance_id")
        .and_then(serde_json::Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
}
/// Which instance answers `get_protocol_info` at `path` (an `observe` read, so it never counts as activity).
pub async fn probe_answering_instance(path: &str) -> Option<String> {
    let answer = crate::host_probe::request_on_socket(
        path,
        &serde_json::json!({"type":"get_protocol_info","observe":true}),
        IDENTITY_PROBE_TIMEOUT_MS,
    )
    .await?;
    answer
        .get("instanceId")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
}
/// Polls the pointer and the endpoint every `SUPERSESSION_POLL_MS`; resolves once they agree `REPLACED_CONFIRMATIONS` times in a row.
pub async fn wait_for_pointer_supersession(
    path: &str,
    pointer: &SupersessionPointer,
    settled: impl Fn() -> bool,
) -> Option<EndpointLoss> {
    let period = std::time::Duration::from_millis(SUPERSESSION_POLL_MS);
    let mut timer = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut watcher = PointerSupersessionWatcher::default();
    loop {
        timer.tick().await;
        if settled() {
            continue;
        }
        let replaced = match read_pointer_instance_id(&pointer.file) {
            Some(named) if named != pointer.instance_id => probe_answering_instance(path)
                .await
                .is_some_and(|answered| answered == named),
            _ => false,
        };
        if let Some(loss) = watcher.observe(replaced, settled()) {
            return Some(loss);
        }
    }
}
/// `watch_for_supersession` for callers that hold a pointer: with no file identity the pointer is the evidence.
pub fn watch_for_supersession_with_pointer(
    path: String,
    identity: Option<crate::socket_ownership::SocketFileIdentity>,
    pointer: Option<SupersessionPointer>,
    settled: impl Fn() -> bool + Send + Sync + 'static,
    on_lost: impl FnOnce(EndpointLoss) + Send + 'static,
) -> SupersessionWatch {
    if identity.is_some() || pointer.is_none() {
        return watch_for_supersession(path, identity, settled, on_lost);
    }
    let task = tokio::spawn(async move {
        if let Some(pointer) = pointer
            && let Some(loss) = wait_for_pointer_supersession(&path, &pointer, settled).await
        {
            on_lost(loss);
        }
    });
    SupersessionWatch { task }
}
pub const ABSENT_CONFIRMATIONS: u32 = 3;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointLoss {
    Replaced,
    Absent,
}
#[derive(Default)]
pub struct SupersessionWatcher {
    absent_polls: u32,
    lost: bool,
}
pub async fn wait_for_supersession(
    path: &str,
    identity: Option<crate::socket_ownership::SocketFileIdentity>,
    settled: impl Fn() -> bool,
) -> Option<EndpointLoss> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let identity = identity?;
        let period = std::time::Duration::from_millis(SUPERSESSION_POLL_MS);
        let mut timer = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut watcher = SupersessionWatcher::default();
        loop {
            timer.tick().await;
            if settled() {
                continue;
            }
            let ownership = match tokio::fs::metadata(path).await {
                Ok(stat) if stat.dev() == identity.dev && stat.ino() == identity.ino => {
                    EndpointOwnership::Held
                }
                Ok(_) => EndpointOwnership::Replaced,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    EndpointOwnership::Absent
                }
                Err(_) => EndpointOwnership::Unknown,
            };
            if let Some(loss) = watcher.observe(ownership, settled()) {
                return Some(loss);
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (path, identity, settled);
        None
    }
}
impl SupersessionWatcher {
    pub fn observe(&mut self, ownership: EndpointOwnership, settled: bool) -> Option<EndpointLoss> {
        if settled || self.lost {
            return None;
        }
        let loss = match ownership {
            EndpointOwnership::Replaced => Some(EndpointLoss::Replaced),
            EndpointOwnership::Absent => {
                self.absent_polls += 1;
                (self.absent_polls >= ABSENT_CONFIRMATIONS).then_some(EndpointLoss::Absent)
            }
            EndpointOwnership::Held | EndpointOwnership::Unknown => {
                self.absent_polls = 0;
                None
            }
        };
        if loss.is_some() {
            self.lost = true;
        }
        loss
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    #[tokio::test]
    async fn watch_reports_a_replaced_endpoint_once() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("socket");
        let original = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let identity = crate::socket_ownership::stat_socket_identity(&path).unwrap();
        let replacement = temp.path().join("new");
        let newer = std::os::unix::net::UnixListener::bind(&replacement).unwrap();
        std::fs::rename(replacement, &path).unwrap();
        let (lost_tx, lost_rx) = tokio::sync::oneshot::channel();
        let watch = watch_for_supersession(
            path.to_string_lossy().into_owned(),
            identity,
            || false,
            move |loss| {
                let _ = lost_tx.send(loss);
            },
        );
        let loss = tokio::time::timeout(std::time::Duration::from_secs(2), lost_rx)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(loss, EndpointLoss::Replaced);
        watch.stop();
        drop((original, newer));
    }
    #[test]
    fn unknown_resets_consecutive_absence_and_loss_latches() {
        let mut watcher = SupersessionWatcher::default();
        assert_eq!(watcher.observe(EndpointOwnership::Absent, false), None);
        watcher.observe(EndpointOwnership::Unknown, false);
        for _ in 0..2 {
            assert_eq!(watcher.observe(EndpointOwnership::Absent, false), None);
        }
        assert_eq!(
            watcher.observe(EndpointOwnership::Absent, false),
            Some(EndpointLoss::Absent)
        );
        assert_eq!(watcher.observe(EndpointOwnership::Replaced, false), None);
    }
    #[test]
    fn pointer_watch_needs_consecutive_agreement_and_never_reports_absent() {
        let mut watcher = PointerSupersessionWatcher::default();
        assert_eq!(watcher.observe(true, false), None);
        assert_eq!(watcher.observe(true, false), None);
        assert_eq!(watcher.observe(false, false), None);
        for _ in 0..2 {
            assert_eq!(watcher.observe(true, false), None);
        }
        assert_eq!(watcher.observe(true, true), None);
        assert_eq!(watcher.observe(true, false), Some(EndpointLoss::Replaced));
        assert_eq!(watcher.observe(true, false), None);
    }
    #[test]
    fn pointer_instance_id_reads_json_and_rejects_garbage() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("pointer");
        assert_eq!(read_pointer_instance_id(&file), None);
        std::fs::write(&file, "{\"instance_id\":\"b\"}").unwrap();
        assert_eq!(read_pointer_instance_id(&file).as_deref(), Some("b"));
        std::fs::write(&file, "{\"instance_id\":\"\"}").unwrap();
        assert_eq!(read_pointer_instance_id(&file), None);
        std::fs::write(&file, "nope").unwrap();
        assert_eq!(read_pointer_instance_id(&file), None);
    }
    #[test]
    fn settled_observation_is_ignored() {
        let mut watcher = SupersessionWatcher::default();
        assert_eq!(watcher.observe(EndpointOwnership::Replaced, true), None);
        assert_eq!(
            watcher.observe(EndpointOwnership::Replaced, false),
            Some(EndpointLoss::Replaced)
        );
    }
}
