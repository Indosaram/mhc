//! Port of senpi packages/coding-agent/src/core/startup-branch-join.ts.

use std::future::Future;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartupBranchError<EPrimary, ESecondary> {
    Primary(EPrimary),
    Secondary(ESecondary),
}

/// Joins two independent startup branches, driving both to settlement so a rejection on one side
/// cannot become an unhandled rejection while the other is still running. When both reject, the
/// primary (model-runtime) reason is returned so error ordering matches the sequential await.
pub async fn join_startup_branches<TPrimary, TSecondary, EPrimary, ESecondary>(
    primary: impl Future<Output = Result<TPrimary, EPrimary>>,
    secondary: impl Future<Output = Result<TSecondary, ESecondary>>,
) -> Result<(TPrimary, TSecondary), StartupBranchError<EPrimary, ESecondary>> {
    let (primary_result, secondary_result) = tokio::join!(primary, secondary);
    let primary_value = primary_result.map_err(StartupBranchError::Primary)?;
    let secondary_value = secondary_result.map_err(StartupBranchError::Secondary)?;
    Ok((primary_value, secondary_value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn both_successes_are_returned() {
        let joined = join_startup_branches(async { Ok::<_, ()>(1) }, async { Ok::<_, ()>("two") }).await;
        assert_eq!(joined.expect("joined"), (1, "two"));
    }

    #[tokio::test]
    async fn a_primary_rejection_is_returned() {
        let joined = join_startup_branches(async { Err::<i32, _>("primary") }, async { Ok::<_, &str>("two") }).await;
        assert_eq!(joined.expect_err("primary error"), StartupBranchError::Primary("primary"));
    }

    #[tokio::test]
    async fn a_secondary_rejection_is_returned_when_the_primary_succeeds() {
        let joined = join_startup_branches(async { Ok::<_, &str>(1) }, async { Err::<i32, _>("secondary") }).await;
        assert_eq!(joined.expect_err("secondary error"), StartupBranchError::Secondary("secondary"));
    }

    #[tokio::test]
    async fn the_primary_reason_wins_when_both_reject() {
        let joined = join_startup_branches(async { Err::<i32, _>("primary") }, async { Err::<i32, _>("secondary") }).await;
        assert_eq!(joined.expect_err("primary wins"), StartupBranchError::Primary("primary"));
    }
}
