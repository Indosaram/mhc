//! Port of `context-lifetime.ts` (senpi#4711 / #4718):
//! Deferred terminal callbacks stop using a retired generation; other failures remain visible.

use maho_ext_api::types::ExtensionContext;

pub fn is_stale_extension_context_error(error: &dyn std::error::Error) -> bool {
    maho_ext_goal::stale_context::is_stale_extension_context_error(error)
}

/// Runs `callback`, retiring this terminal generation if the host rejects a stale extension
/// context; unrelated errors are returned unchanged.
pub fn with_terminal_context<T, E: std::error::Error>(
    callback: impl FnOnce() -> Result<T, E>,
    retire: impl FnOnce(),
) -> Result<Option<T>, E> {
    match callback() {
        Ok(value) => Ok(Some(value)),
        Err(error) => {
            if is_stale_extension_context_error(&error) {
                retire();
                Ok(None)
            } else {
                Err(error)
            }
        }
    }
}

/// Checks whether `ctx`'s session generation is still active; if a stale-context failure is
/// reported, runs `retire` and returns `false`.
pub fn assert_context_active_or_retire(
    ctx: Option<&ExtensionContext>,
    retire: impl FnOnce(),
) -> bool {
    let Some(actions) = ctx.and_then(|ctx| ctx.session_manager.extension_context_actions()) else {
        return true;
    };
    matches!(
        with_terminal_context(|| actions.assert_active(), retire),
        Ok(Some(()))
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use maho_ext_api::types::ExtensionFailure;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn stale_context_error_retires_and_returns_none() {
        let retired = AtomicUsize::new(0);
        let result: Result<Option<()>, ExtensionFailure> = with_terminal_context(
            || {
                Err(ExtensionFailure::new(format!(
                    "{} test",
                    maho_ext_goal::stale_context::STALE_EXTENSION_CONTEXT_ERROR_PREFIX
                )))
            },
            || {
                retired.fetch_add(1, Ordering::SeqCst);
            },
        );
        assert_eq!(result.expect("swallowed stale error"), None);
        assert_eq!(retired.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn unrelated_error_propagates_without_retiring() {
        let retired = AtomicUsize::new(0);
        let result: Result<Option<()>, ExtensionFailure> = with_terminal_context(
            || Err(ExtensionFailure::new("permission denied")),
            || {
                retired.fetch_add(1, Ordering::SeqCst);
            },
        );
        assert_eq!(
            result.expect_err("unrelated error escapes").to_string(),
            "permission denied"
        );
        assert_eq!(retired.load(Ordering::SeqCst), 0);
    }
}
