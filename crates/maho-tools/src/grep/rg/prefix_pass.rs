use super::enumerate::Candidate;
use crate::{
    definition::AbortSignal,
    grep::engine::{GrepEngineError, GrepEngineResult},
};
use std::time::Instant;
use tokio::io::AsyncReadExt;
pub const MAX_FILE_BYTES: usize = 4_194_304;
pub async fn read_searchable_prefix(
    candidate: &Candidate,
    result: &mut GrepEngineResult,
    deadline: Instant,
    signal: &AbortSignal,
) -> Result<Option<Vec<u8>>, GrepEngineError> {
    super::process::check(deadline, signal)?;
    let read = async {
        let mut file = tokio::fs::File::open(&candidate.absolute).await?;
        let mut data = vec![0; MAX_FILE_BYTES];
        let mut length = 0;
        while length < data.len() {
            super::process::check(deadline, signal)?;
            let count = file.read(&mut data[length..]).await?;
            if count == 0 {
                break;
            }
            length += count;
        }
        data.truncate(length);
        Ok::<_, PrefixReadError>(data)
    }
    .await;
    super::process::check(deadline, signal)?;
    let mut prefix = match read {
        Ok(data) => data,
        Err(PrefixReadError::Search(error)) => return Err(error),
        Err(PrefixReadError::Io(error)) => {
            result.skipped_oversized += 1;
            result.warnings.push(maho_grep::GrepWarning {
                path: Some(candidate.display.clone()),
                code: "SKIPPED_OVERSIZED".into(),
                message: error.to_string(),
            });
            return Ok(None);
        }
    };
    if prefix.contains(&0) {
        result.skipped_binary += 1;
        return Ok(None);
    }
    let Some(newline) = prefix.iter().rposition(|b| *b == b'\n') else {
        result.skipped_oversized += 1;
        return Ok(None);
    };
    prefix.truncate(newline + 1);
    Ok(Some(prefix))
}

#[derive(Debug, thiserror::Error)]
enum PrefixReadError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Search(#[from] GrepEngineError),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_result() -> GrepEngineResult {
        GrepEngineResult {
            matches: Vec::new(),
            file_counts: Vec::new(),
            counts: maho_grep::GrepCounts {
                matches: Some(0),
                files: 0,
                exact: true,
            },
            files_searched: 0,
            limit_reached: false,
            per_file_limit_reached: false,
            skipped_oversized: 0,
            prefix_searched: 0,
            skipped_binary: 0,
            missing_paths: Vec::new(),
            warnings: Vec::new(),
            timed_out: false,
            elapsed_ms: 0.0,
            effective_pattern: String::new(),
            pattern_kind: "regex".into(),
            regex_engine: "rust".into(),
        }
    }

    #[tokio::test]
    async fn aborted_prefix_does_not_classify_missing_file() {
        let signal = AbortSignal::default();
        signal.abort();
        let candidate = Candidate {
            absolute: "missing".into(),
            display: "missing".into(),
            size: 5_000_000,
            ordinal: 1,
        };
        let mut result = empty_result();
        let read = read_searchable_prefix(
            &candidate,
            &mut result,
            Instant::now() + std::time::Duration::from_secs(60),
            &signal,
        )
        .await;
        assert!(matches!(read, Err(GrepEngineError::Aborted)));
        assert_eq!(result.skipped_oversized, 0);
        assert!(result.warnings.is_empty());
    }

    #[tokio::test]
    async fn expired_prefix_does_not_classify_missing_file() {
        let candidate = Candidate {
            absolute: "missing".into(),
            display: "missing".into(),
            size: 5_000_000,
            ordinal: 1,
        };
        let mut result = empty_result();
        let read = read_searchable_prefix(
            &candidate,
            &mut result,
            Instant::now(),
            &AbortSignal::default(),
        )
        .await;
        assert!(
            matches!(read, Err(GrepEngineError::EngineUnavailable(message)) if message == "TIMED_OUT")
        );
        assert_eq!(result.skipped_oversized, 0);
        assert!(result.warnings.is_empty());
    }
}
