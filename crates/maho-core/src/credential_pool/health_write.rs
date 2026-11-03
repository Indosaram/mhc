//! Port of senpi packages/coding-agent/src/core/credential-pool/health-write.ts.

use std::future::Future;

use crate::hidden_stdout_log::append_debug_log_entry;
use crate::lockfile_policy::CredentialStoreBusyError;

/// The write was skipped because the state file stayed locked past its bound (upstream `STORE_BUSY`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoreBusy;

/// Errors a health write can report: a busy store is singled out so it can be skipped.
pub trait HealthWriteError {
    fn store_busy(&self) -> Option<&CredentialStoreBusyError>;
}

impl HealthWriteError for CredentialStoreBusyError {
    fn store_busy(&self) -> Option<&CredentialStoreBusyError> {
        Some(self)
    }
}

/// Runs a pool health write that must never decide the request's outcome (senpi#3285). Health is
/// advisory: when the state file stays locked past its bound, the write is skipped and logged, and the
/// request carries on. Every other failure still propagates.
pub async fn write_health_unless_store_busy<T, E, F>(
    what: &str,
    write: F,
) -> Result<Result<T, StoreBusy>, E>
where
    E: HealthWriteError,
    F: Future<Output = Result<T, E>>,
{
    match write.await {
        Ok(value) => Ok(Ok(value)),
        Err(error) => {
            let Some(busy) = error.store_busy() else {
                return Err(error);
            };
            append_debug_log_entry(
                "credential pool: skipped a health write, state file busy",
                &format!(
                    "{what}: lock {} still held after {}ms",
                    busy.path, busy.waited_ms
                ),
            );
            Ok(Err(StoreBusy))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq)]
    enum Failure {
        Busy(CredentialStoreBusyError),
        Other(&'static str),
    }

    impl HealthWriteError for Failure {
        fn store_busy(&self) -> Option<&CredentialStoreBusyError> {
            match self {
                Self::Busy(error) => Some(error),
                Self::Other(_) => None,
            }
        }
    }

    #[tokio::test]
    async fn skips_a_write_the_busy_store_refused_and_propagates_every_other_failure() {
        let ok = write_health_unless_store_busy("success", async { Ok::<_, Failure>(7) }).await;
        assert_eq!(ok, Ok(Ok(7)));
        let busy = write_health_unless_store_busy("block", async {
            Err::<(), _>(Failure::Busy(CredentialStoreBusyError::new(
                "/tmp/state.json",
                5000,
            )))
        })
        .await;
        assert_eq!(busy, Ok(Err(StoreBusy)));
        let other =
            write_health_unless_store_busy("block", async { Err::<(), _>(Failure::Other("EIO")) })
                .await;
        assert_eq!(other, Err(Failure::Other("EIO")));
    }
}
