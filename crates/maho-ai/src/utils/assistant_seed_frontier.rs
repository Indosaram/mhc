//! Port of senpi packages/ai/src/utils/assistant-seed-frontier.ts.

/// `SeedFrontierError.code`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeedFrontierErrorCode {
    MissingNextOffset,
    InvalidNextOffset,
}

impl SeedFrontierErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MissingNextOffset => "missing_next_offset",
            Self::InvalidNextOffset => "invalid_next_offset",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("Block seed requires nextOffset equal to zero or its full UTF-16 length")]
pub struct SeedFrontierError {
    pub code: SeedFrontierErrorCode,
}

impl SeedFrontierError {
    pub fn new(next_offset: Option<i64>) -> Self {
        Self {
            code: if next_offset.is_none() {
                SeedFrontierErrorCode::MissingNextOffset
            } else {
                SeedFrontierErrorCode::InvalidNextOffset
            },
        }
    }
}

/// No inference at a consumer: only full replay (0) or full continuation (the seed's UTF-16
/// length) is supported. Signed input so a negative producer frontier is rejected, as in TS.
pub fn seed_next_offset(
    next_offset: Option<i64>,
    seed_length: usize,
) -> Result<usize, SeedFrontierError> {
    match next_offset {
        Some(0) => Ok(0),
        Some(offset) if usize::try_from(offset).is_ok_and(|offset| offset == seed_length) => {
            Ok(seed_length)
        }
        other => Err(SeedFrontierError::new(other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_full_replay_or_full_continuation() {
        assert_eq!(seed_next_offset(Some(0), 2), Ok(0));
        assert_eq!(seed_next_offset(Some(2), 2), Ok(2));
        for offset in [-1, 1, 3] {
            assert_eq!(
                seed_next_offset(Some(offset), 2).map_err(|error| error.code),
                Err(SeedFrontierErrorCode::InvalidNextOffset)
            );
        }
        assert_eq!(
            seed_next_offset(None, 2).map_err(|error| error.code.as_str()),
            Err("missing_next_offset")
        );
    }
}
