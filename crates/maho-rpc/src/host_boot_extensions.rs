//! A multi-session host loads its extensions per `open_session` or `warm`. A launch that hands it
//! a private descriptor table expects an extension to claim its channel with no session open, so such
//! a host runs the warm of its default profile once at boot (senpi#6147). Without a table nothing
//! changes.

use crate::host_warm::{HostWarm, HostWarmProfile};

pub const PRIVATE_FDS_TABLE_ENV: &str = "SENPI_PRIVATE_FDS";

/// Whether a private descriptor table was handed to this process through its environment.
pub fn private_table_present() -> bool {
    std::env::var(PRIVATE_FDS_TABLE_ENV).is_ok_and(|value| !value.is_empty())
}

/// Starts the boot load and returns at once: it never delays the listener, and a failure is only logged.
pub fn warm_at_boot_for_private_fds(
    warm: Option<&HostWarm>,
    profile: HostWarmProfile,
    table_present: bool,
    log: impl FnOnce(String) + Send + 'static,
) -> Option<tokio::task::JoinHandle<()>> {
    if !table_present {
        return None;
    }
    let warm = warm?.clone();
    Some(tokio::spawn(async move {
        if let Err(cause) = warm(profile).await {
            log(format!("boot extension load failed: {cause}"));
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::future::FutureExt;
    use std::sync::{Arc, Mutex, PoisonError};

    #[tokio::test]
    async fn warms_only_when_private_table_is_present_and_logs_errors() {
        let called = Arc::new(Mutex::new(0usize));
        let counter = called.clone();
        let warm: HostWarm = Arc::new(move |_profile| {
            let counter = counter.clone();
            async move {
                *counter.lock().unwrap_or_else(PoisonError::into_inner) += 1;
                Err("boom".to_owned())
            }
            .boxed()
        });

        assert!(
            warm_at_boot_for_private_fds(Some(&warm), HostWarmProfile::default(), false, |_| {})
                .is_none()
        );
        assert_eq!(*called.lock().unwrap_or_else(PoisonError::into_inner), 0);

        let logs = Arc::new(Mutex::new(Vec::new()));
        let sink = logs.clone();
        let handle = warm_at_boot_for_private_fds(
            Some(&warm),
            HostWarmProfile {
                cwd: "/work".into(),
                ..Default::default()
            },
            true,
            move |msg| {
                sink.lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(msg);
            },
        )
        .expect("spawned");
        handle.await.expect("join");
        assert_eq!(*called.lock().unwrap_or_else(PoisonError::into_inner), 1);
        assert_eq!(
            logs.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .as_slice(),
            ["boot extension load failed: boom"]
        );
    }
}
