//! A successor's startup, driven by its launcher (senpi#4602, A-amended sections 1 and 4, N1).
//!
//! The successor's path is P0..P5, then P7 (presenter), then P6 (publication), then P8 (public
//! answer). The presenter exchange starts only when P7 is entered, after internal and observer
//! readiness, while the predecessor still serves its unchanged public endpoint. Publication is
//! granted only after a validated P7 result: an enrollment that is rejected, closes or expires ends
//! the candidate before it ever touched the predecessor's public name, key or pointer.

use crate::host_protocol_info::HostProtocolInfo;
use crate::host_startup_policy::{
    CompleteResult, StartupController, StartupPhase, startup_timeout_detail,
};
use crate::host_startup_run::StartupFailure;
use crate::host_startup_wire::{ADOPTION_GRANT_FILE, startup_grant_line};
use crate::mcp_presenter_bootstrap::{PresenterLaunch, relay_presenter_bootstrap};
use crate::mcp_presenter_wire::BoxedPrivateStream;
use futures::future::{BoxFuture, FutureExt, Shared};
use serde_json::json;
use std::future::Future;
use std::io;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

/// Capability advertised by a predecessor that participates in the endpoint owner-token fence (senpi#4602 Q1).
pub const ENDPOINT_OWNER_FENCE_CAPABILITY: &str = "endpoint_owner_fence";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresenterResult {
    Enrolled,
    Unavailable,
    Closed,
    TimedOut,
}

type SuccessorGrantFn = Box<dyn FnOnce() -> BoxFuture<'static, Result<(), String>> + Send>;
type AdoptionWritePipeFn = Box<dyn FnOnce(String) -> Result<(), String> + Send>;

pub struct SuccessorAttempt {
    pub controller: Arc<StartupController>,
    pub coordinated: bool,
    grant_fn: Mutex<Option<SuccessorGrantFn>>,
}

impl SuccessorAttempt {
    pub fn new(
        controller: Arc<StartupController>,
        coordinated: bool,
        grant: impl FnOnce() -> BoxFuture<'static, Result<(), String>> + Send + 'static,
    ) -> Self {
        Self {
            controller,
            coordinated,
            grant_fn: Mutex::new(Some(Box::new(grant))),
        }
    }

    pub async fn grant(&self) -> Result<(), String> {
        let action = self
            .grant_fn
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        match action {
            Some(f) => f().await,
            None => Ok(()),
        }
    }
}

#[allow(clippy::large_enum_variant)]
pub enum SuccessorStartupResult {
    Ready { answer: HostProtocolInfo },
    NotReady { outcome: StartupFailure },
}

pub async fn drive_successor_startup<P, PFut, A, AFut>(
    attempt: &SuccessorAttempt,
    presenter: Option<P>,
    await_answer: A,
) -> SuccessorStartupResult
where
    P: FnOnce() -> PFut,
    PFut: Future<Output = PresenterResult>,
    A: FnOnce(u64) -> AFut,
    AFut: Future<Output = Option<HostProtocolInfo>>,
{
    let controller = &attempt.controller;
    let failure = || async {
        let settled = controller.settled().await;
        match StartupFailure::from_outcome(settled) {
            Ok(outcome) => SuccessorStartupResult::NotReady { outcome },
            Err(_) => SuccessorStartupResult::NotReady {
                outcome: StartupFailure::Failed {
                    phase: controller.phase(),
                    cause: "startup controller reported READY on a failure path".to_owned(),
                    elapsed_ms: 0,
                },
            },
        }
    };

    if attempt.coordinated {
        if !controller.when_phase(StartupPhase::PresenterResolved).await {
            return failure().await;
        }
        if let Some(run_presenter) = presenter {
            // LEAD ruling 10-31 (b): a channel that closes without enrolling passes P7 as
            // "not enrolled" - preserves #4254's tested behaviour. Only P7's own expiry fails.
            let terminal = tokio::select! {
                biased;
                _ = controller.settled() => true,
                _ = run_presenter() => false,
            };
            if terminal {
                return failure().await;
            }
        }
        if controller.complete(StartupPhase::PresenterResolved) != CompleteResult::Accepted {
            return failure().await;
        }
        if let Err(cause) = attempt.grant().await {
            controller.fail(cause);
            return failure().await;
        }
        if !controller.when_phase(StartupPhase::PublicVerified).await {
            return failure().await;
        }
    }

    let remaining = controller.remaining_ms().max(1);
    let answer = tokio::select! {
        biased;
        _ = controller.settled() => None,
        ans = await_answer(remaining) => ans,
    };
    if let Some(answer) = answer
        && controller.complete(StartupPhase::PublicVerified) == CompleteResult::Accepted
    {
        return SuccessorStartupResult::Ready { answer };
    }
    if controller.terminal().is_none() {
        controller.fail("the successor never answered on the public socket");
    }
    failure().await
}

pub enum PresenterBootstrapSource {
    Eager(BoxedPrivateStream),
    Deferred(Box<dyn FnOnce() -> BoxFuture<'static, Option<BoxedPrivateStream>> + Send>),
}

/// The presenter exchange of one successor. A coordinated successor whose owner channel is DEFERRED
/// asks for the fresh permit only when called, at its P7. An eager owner channel, and every channel
/// of an uncoordinated launch, is relayed from the spawn as before; calling the result joins that relay.
pub fn successor_presenter(
    bootstrap: Option<PresenterBootstrapSource>,
    downstream: Option<BoxedPrivateStream>,
    predecessor: &HostProtocolInfo,
    coordinated: bool,
) -> Option<Box<dyn FnOnce() -> BoxFuture<'static, PresenterResult> + Send>> {
    let bootstrap = bootstrap?;
    let pred_instance = predecessor.instance_id.clone().map(Some);
    let is_deferred = matches!(bootstrap, PresenterBootstrapSource::Deferred(_));
    let mut source = Some(bootstrap);
    let mut down = downstream;

    let run_relay = move || -> BoxFuture<'static, PresenterResult> {
        let src = source.take();
        let down = down.take();
        async move {
            let upstream = match src {
                Some(PresenterBootstrapSource::Eager(s)) => Some(s),
                Some(PresenterBootstrapSource::Deferred(f)) => f().await,
                None => None,
            };
            let (Some(upstream), Some(downstream)) = (upstream, down) else {
                return PresenterResult::Closed;
            };
            relay_presenter_bootstrap(
                upstream,
                downstream,
                Some(PresenterLaunch {
                    kind: "replacement",
                    predecessor_host_instance: pred_instance,
                }),
                std::future::pending(),
            )
            .await;
            PresenterResult::Enrolled
        }
        .boxed()
    };

    if !is_deferred || !coordinated {
        let shared: Shared<BoxFuture<'static, PresenterResult>> = run_relay().shared();
        return Some(Box::new(move || shared.boxed()));
    }
    Some(Box::new(run_relay))
}

/// The refusal detail of a failed successor attempt, in the same machine-readable form an initial start reports.
pub fn failure_detail(outcome: &StartupFailure, cleanup_failure: Option<&str>) -> String {
    match outcome {
        StartupFailure::Timeout(timeout) => startup_timeout_detail(
            timeout,
            if cleanup_failure.is_some() {
                "incomplete"
            } else {
                "completed"
            },
        ),
        StartupFailure::Failed { cause, .. } => cause.clone(),
    }
}

pub struct AdoptionGrantHandle {
    file: std::path::PathBuf,
    attempt: String,
    write_pipe: Mutex<Option<AdoptionWritePipeFn>>,
}

/// The grant for one attempt: authority in the candidate's own generation directory, wake-up on its startup pipe.
pub fn adoption_grant(
    attempt: String,
    generation_dir: &Path,
    write_pipe: impl FnOnce(String) -> Result<(), String> + Send + 'static,
) -> AdoptionGrantHandle {
    AdoptionGrantHandle {
        file: generation_dir.join(ADOPTION_GRANT_FILE),
        attempt,
        write_pipe: Mutex::new(Some(Box::new(write_pipe))),
    }
}

impl AdoptionGrantHandle {
    pub fn grant(&self) -> Result<(), String> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&self.file)
            .map_err(|err| format!("failed to write adoption grant: {err}"))?;
        let content = format!("{}\n", json!({ "attempt": self.attempt }));
        std::io::Write::write_all(&mut file, content.as_bytes())
            .map_err(|err| format!("failed to write adoption grant: {err}"))?;

        let writer = self
            .write_pipe
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
            .ok_or_else(|| "the successor's startup pipe is gone".to_owned())?;
        writer(startup_grant_line(&self.attempt))
    }

    pub fn revoke(&self) -> io::Result<()> {
        match std::fs::remove_file(&self.file) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            result => result,
        }
    }
}

/// Q1 (LEAD ruling, senpi#4602): the owner-token guard is claimed only against a predecessor that advertises it.
pub fn predecessor_owner_args(host: &HostProtocolInfo) -> Vec<String> {
    if let Some(instance_id) = &host.instance_id
        && host
            .capabilities
            .iter()
            .any(|cap| cap == ENDPOINT_OWNER_FENCE_CAPABILITY)
    {
        vec!["--replace-owner".to_owned(), instance_id.clone()]
    } else {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host_startup_policy::tests::VirtualClock;
    use crate::host_startup_policy::{StartupLaunchKind, start_startup_controller};

    fn host_info(instance_id: Option<&str>, caps: &[&str]) -> HostProtocolInfo {
        crate::host_protocol_info::parse_host_protocol_info(&json!({
            "protocolVersion": 1,
            "serverVersion": "1",
            "capabilities": caps,
            "instanceId": instance_id,
        }))
        .expect("info")
    }

    #[tokio::test]
    async fn coordinated_successor_runs_presenter_before_grant_and_public_verification() {
        let clock = VirtualClock::new();
        let ctrl = start_startup_controller(StartupLaunchKind::Successor, clock, None);
        for phase in [
            StartupPhase::SupervisorBootstrap,
            StartupPhase::SupervisorSetup,
            StartupPhase::HostBootstrap,
            StartupPhase::HostRuntime,
            StartupPhase::InternalReady,
            StartupPhase::ObserverReady,
        ] {
            assert_eq!(ctrl.complete(phase), CompleteResult::Accepted);
        }
        assert_eq!(ctrl.phase(), StartupPhase::PresenterResolved);

        let granted = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let granted_flag = granted.clone();
        let ctrl_for_grant = ctrl.clone();
        let attempt = SuccessorAttempt::new(ctrl.clone(), true, move || {
            granted_flag.store(true, std::sync::atomic::Ordering::SeqCst);
            assert_eq!(
                ctrl_for_grant.complete(StartupPhase::PublicPublished),
                CompleteResult::Accepted
            );
            Box::pin(async { Ok(()) })
        });

        let expected_answer = host_info(Some("succ"), &[ENDPOINT_OWNER_FENCE_CAPABILITY]);
        let ans_clone = expected_answer.clone();
        let res = drive_successor_startup(
            &attempt,
            Some(|| async { PresenterResult::Enrolled }),
            |_| async move { Some(ans_clone) },
        )
        .await;
        assert!(granted.load(std::sync::atomic::Ordering::SeqCst));
        assert!(
            matches!(res, SuccessorStartupResult::Ready { answer } if answer == expected_answer)
        );
    }

    #[test]
    fn adoption_grant_and_predecessor_owner_args_work() {
        let temp = tempfile::tempdir().expect("tempdir");
        let sent: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let sink = sent.clone();
        let grant = adoption_grant("att-9".to_owned(), temp.path(), move |line| {
            *sink.lock().unwrap_or_else(PoisonError::into_inner) = Some(line);
            Ok(())
        });
        grant.grant().expect("grant");
        assert!(temp.path().join(ADOPTION_GRANT_FILE).exists());
        assert_eq!(
            sent.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .as_deref(),
            Some(startup_grant_line("att-9").as_str())
        );
        grant.revoke().expect("revoke");
        assert!(!temp.path().join(ADOPTION_GRANT_FILE).exists());

        let with_cap = host_info(Some("old-1"), &[ENDPOINT_OWNER_FENCE_CAPABILITY]);
        assert_eq!(
            predecessor_owner_args(&with_cap),
            vec!["--replace-owner", "old-1"]
        );
        let without_cap = host_info(Some("old-1"), &[]);
        assert!(predecessor_owner_args(&without_cap).is_empty());
    }
}
