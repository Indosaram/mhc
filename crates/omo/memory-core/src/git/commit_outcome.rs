//! Resolve a timed-out commit by its serialized HEAD transition.
use super::{errors::GitError, exec::GitExecResult};

pub fn landed_after_timeout(
    head_before: Option<&str>,
    message: &str,
    head: impl FnOnce() -> Result<Option<String>, GitError>,
    result: impl Fn(&[String]) -> Result<GitExecResult, GitError>,
) -> Result<Option<String>, GitError> {
    let Some(head) = head()? else { return Ok(None) };
    if Some(head.as_str()) == head_before {
        return Ok(None);
    }
    let parent = result(&[
        "rev-parse".into(),
        "--verify".into(),
        "--quiet".into(),
        format!("{head}^"),
    ])?;
    let parent_sha = (parent.code == 0).then(|| parent.stdout.trim());
    if parent_sha != head_before {
        return Ok(None);
    }
    let mut args = super::stable_log_output::stable_log_output();
    args.extend([
        "log".into(),
        "-1".into(),
        "--format=%s".into(),
        head.clone(),
    ]);
    let subject = result(&args)?;
    Ok(
        (subject.code == 0 && subject.stdout.trim() == message.lines().next().unwrap_or("").trim())
            .then_some(head),
    )
}

pub fn commit_judged_by_outcome<T>(
    commit: impl FnOnce() -> Result<T, GitError>,
    head: impl Fn() -> Result<Option<String>, GitError>,
    result: impl Fn(&[String]) -> Result<GitExecResult, GitError>,
    message: &str,
) -> Result<Option<String>, GitError> {
    let before = head()?;
    match commit() {
        Ok(_) => Ok(None),
        Err(error) => {
            if !matches!(&error, GitError::Timeout { .. })
                && !matches!(&error, GitError::Io(e) if e.kind() == std::io::ErrorKind::TimedOut)
            {
                return Err(error);
            }
            landed_after_timeout(before.as_deref(), message, head, result)?
                .map(Some)
                .ok_or(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    fn timeout() -> Result<(), GitError> {
        Err(GitError::Io(std::io::Error::from(
            std::io::ErrorKind::TimedOut,
        )))
    }
    #[test]
    fn timeout_that_landed_returns_sha() {
        let reads = Cell::new(0);
        let result = commit_judged_by_outcome(
            timeout,
            || {
                let n = reads.get();
                reads.set(n + 1);
                Ok(Some(if n == 0 { "before" } else { "after" }.into()))
            },
            |args| {
                Ok(GitExecResult {
                    code: 0,
                    stdout: if args[0] == "rev-parse" {
                        "before\n"
                    } else {
                        "write\n"
                    }
                    .into(),
                    stderr: String::new(),
                })
            },
            "write\n\nbody",
        )
        .unwrap();
        assert_eq!(result.as_deref(), Some("after"));
    }
    #[test]
    fn unchanged_head_and_wrong_subject_do_not_claim_commit() {
        assert!(
            commit_judged_by_outcome(
                timeout,
                || Ok(Some("before".into())),
                |_| unreachable!(),
                "write"
            )
            .is_err()
        );
        assert_eq!(
            landed_after_timeout(
                Some("before"),
                "write",
                || Ok(Some("after".into())),
                |args| Ok(GitExecResult {
                    code: 0,
                    stdout: if args[0] == "rev-parse" {
                        "before"
                    } else {
                        "another write"
                    }
                    .into(),
                    stderr: String::new()
                })
            )
            .unwrap(),
            None
        );
    }
}
