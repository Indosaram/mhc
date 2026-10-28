use super::*;
use crate::sync::engine::ports::*;

struct Never;
impl Transport for Never {
    fn pull(&self, _: u64, _: usize) -> PortFuture<'_, PullPage> {
        Box::pin(async { Err(EngineError::Network) })
    }
    fn download<'a>(&'a self, _: &'a str) -> PortFuture<'a, Vec<u8>> {
        Box::pin(async { Err(EngineError::Network) })
    }
    fn reserve<'a>(&'a self, _: usize, _: &'a str) -> PortFuture<'a, String> {
        Box::pin(async { Err(EngineError::Network) })
    }
    fn upload<'a>(&'a self, _: &'a str, _: &'a [u8]) -> PortFuture<'a, ()> {
        Box::pin(async { Err(EngineError::Network) })
    }
    fn append(&self, _: AppendRequest) -> PortFuture<'_, AppendResult> {
        Box::pin(async { Err(EngineError::Network) })
    }
    fn subscribe(&self, _: StreamHandlers) -> Unsubscribe {
        Box::new(|| {})
    }
}
impl Verifier for Never {
    fn verify_entry<'a>(&'a self, _: &'a PulledEntry, _: &'a [u8]) -> PortFuture<'a, Verdict> {
        Box::pin(async { Ok(Verdict::Refused(RefusalCode::Malformed)) })
    }
    fn verify_snapshot<'a>(
        &'a self,
        _: &'a SnapshotRef,
        _: &'a [Vec<u8>],
    ) -> PortFuture<'a, Verdict> {
        Box::pin(async { Ok(Verdict::Refused(RefusalCode::Malformed)) })
    }
}
impl Sealer for Never {
    fn seal<'a>(&'a self, _: &'a str, _: u64, _: &'a BatchPayload) -> PortFuture<'a, SealedBatch> {
        Box::pin(async { Err(EngineError::Network) })
    }
}

fn options(dir: &std::path::Path) -> OpenDeviceSyncOptions {
    let never = Rc::new(Never);
    OpenDeviceSyncOptions {
        on_status_change: None,
        repo_dir: dir.join("repo"),
        state_dir: dir.join("state"),
        locks_dir: dir.join("locks"),
        session: SyncSession {
            transport: never.clone(),
            verifier: never.clone(),
            sealer: never,
        },
        machine_id: Some("fixed".into()),
        log: None,
    }
}

#[test]
fn a_second_open_is_refused_until_the_first_closes_and_close_is_idempotent()
-> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    std::fs::create_dir_all(dir.path().join("repo/.git"))?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()?;
    let first = open_device_sync(options(dir.path()))?.ok_or("first open should lease")?;
    assert!(open_device_sync(options(dir.path()))?.is_none());
    assert_eq!(first.status()?, EngineStatus::NeverSynced);
    runtime.block_on(first.close())?;
    runtime.block_on(first.close())?;
    let again = open_device_sync(options(dir.path()))?.ok_or("lease should be free after close")?;
    runtime.block_on(again.close())?;
    Ok(())
}

#[test]
fn offline_status_threshold_notifications_and_stop() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let repo = dir.path().join("repo");
    std::fs::create_dir_all(&repo)?;
    let output = std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&repo)
        .output()?;
    assert!(output.status.success());
    let notices = Rc::new(Cell::new(0));
    let n = notices.clone();
    let mut opts = options(dir.path());
    opts.on_status_change = Some(Rc::new(move || n.set(n.get() + 1)));
    let device = open_device_sync(opts)?.ok_or("lease")?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()?;
    runtime.block_on(async {
        for attempt in 1..=3 {
            assert!(device.engine.run_cycle().await.is_err());
            if attempt < 3 {
                assert_eq!(device.status()?, EngineStatus::NeverSynced);
            } else {
                assert!(matches!(
                    device.status()?,
                    EngineStatus::Failing { attempts: 3, .. }
                ));
            }
        }
        assert_eq!(notices.get(), 1);
        assert!(device.engine.run_cycle().await.is_err());
        assert_eq!(notices.get(), 1);
        device.close().await?;
        assert!(device.engine.run_cycle().await?.waiting);
        Ok::<_, EngineError>(())
    })?;
    Ok(())
}
