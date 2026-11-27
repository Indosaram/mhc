//! senpi `modes/rpc/compact-admission-storage.ts`: bounded per-admission row files in the
//! epoch directory (128 rows / 16 MiB, <= 131,584 bytes per row).

use crate::compact_admission_error::{CompactAdmissionError, CompactAdmissionReason};
use crate::compact_recovery_ids::parse_content_digest;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

pub struct AdmissionLimits {
    pub entries: usize,
    pub bytes: usize,
    pub hot: usize,
    pub row: usize,
    pub retired: usize,
}

/// Per socket epoch (RULING-113): 16 live producers; terminal rows in an LRU of 128 rows / 16 MiB
/// reserved disk bytes; an evicted ID keeps only its digest in a 4,096-entry ring.
pub const ADMISSION_LIMITS: AdmissionLimits = AdmissionLimits {
    entries: 128,
    bytes: 16 * 1024 * 1024,
    hot: 16,
    row: 131_584,
    retired: 4096,
};

pub const MAX_ADMISSION_ANSWER_BYTES: usize = 65_536;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdmissionState {
    Pending,
    Complete,
    Cancelled,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdmissionRow {
    pub fingerprint: String,
    pub state: AdmissionState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
}

impl AdmissionRow {
    pub fn validate(&self) -> Result<(), CompactAdmissionError> {
        if parse_content_digest(&self.fingerprint).is_err() {
            return Err(CompactAdmissionError::new(
                CompactAdmissionReason::StorageUnavailable,
            ));
        }
        if let Some(failure) = &self.failure
            && failure != "storage_unavailable"
        {
            return Err(CompactAdmissionError::new(
                CompactAdmissionReason::StorageUnavailable,
            ));
        }
        if let Some(answer) = &self.answer
            && answer.len() > MAX_ADMISSION_ANSWER_BYTES
        {
            return Err(CompactAdmissionError::new(
                CompactAdmissionReason::StorageUnavailable,
            ));
        }
        Ok(())
    }
}

pub async fn read_admission_row(
    directory: &Path,
    failures: Option<&HashMap<String, AdmissionRow>>,
    key: &str,
) -> Result<Option<AdmissionRow>, CompactAdmissionError> {
    if let Some(failure) = failures.and_then(|map| map.get(key)) {
        return Ok(Some(failure.clone()));
    }
    let path = directory.join(key);
    let text = match tokio::fs::read_to_string(&path).await {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => {
            return Err(CompactAdmissionError::new(
                CompactAdmissionReason::StorageUnavailable,
            ));
        }
    };
    if text.len() > ADMISSION_LIMITS.row {
        return Err(CompactAdmissionError::new(
            CompactAdmissionReason::StorageUnavailable,
        ));
    }
    let row: AdmissionRow = serde_json::from_str(&text)
        .map_err(|_| CompactAdmissionError::new(CompactAdmissionReason::StorageUnavailable))?;
    row.validate()?;
    Ok(Some(row))
}

pub async fn write_admission_row(
    directory: &Path,
    key: &str,
    row: &AdmissionRow,
) -> Result<(), CompactAdmissionError> {
    row.validate()?;
    let text = serde_json::to_string(row)
        .map_err(|_| CompactAdmissionError::new(CompactAdmissionReason::StorageUnavailable))?;
    if text.len() > ADMISSION_LIMITS.row {
        return Err(CompactAdmissionError::new(
            CompactAdmissionReason::StorageUnavailable,
        ));
    }
    let path = directory.join(key);
    let write_res = async {
        use tokio::io::AsyncWriteExt;
        let mut opts = tokio::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        opts.mode(0o600);
        let mut file = opts.open(&path).await?;
        file.write_all(text.as_bytes()).await?;
        file.sync_all().await
    }
    .await;
    if write_res.is_err() {
        return Err(CompactAdmissionError::new(
            CompactAdmissionReason::StorageUnavailable,
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn writes_reads_and_validates_admission_rows() {
        let dir = tempfile::tempdir().expect("tempdir");
        let fp = "a".repeat(64);
        assert_eq!(
            read_admission_row(dir.path(), None, "k1")
                .await
                .expect("missing"),
            None
        );

        let row = AdmissionRow {
            fingerprint: fp.clone(),
            state: AdmissionState::Complete,
            failure: None,
            answer: Some("{\"ok\":true}".into()),
        };
        write_admission_row(dir.path(), "k1", &row)
            .await
            .expect("write");
        let loaded = read_admission_row(dir.path(), None, "k1")
            .await
            .expect("read")
            .expect("present");
        assert_eq!(loaded, row);

        let mut failures = HashMap::new();
        failures.insert(
            "k1".into(),
            AdmissionRow {
                fingerprint: fp,
                state: AdmissionState::Failed,
                failure: Some("storage_unavailable".into()),
                answer: None,
            },
        );
        let from_mem = read_admission_row(dir.path(), Some(&failures), "k1")
            .await
            .expect("mem")
            .expect("present");
        assert_eq!(from_mem.state, AdmissionState::Failed);
    }

    #[tokio::test]
    async fn rejects_oversized_or_malformed_rows() {
        let dir = tempfile::tempdir().expect("tempdir");
        let bad = AdmissionRow {
            fingerprint: "not-hex".into(),
            state: AdmissionState::Pending,
            failure: None,
            answer: None,
        };
        assert_eq!(
            write_admission_row(dir.path(), "bad", &bad)
                .await
                .expect_err("bad fingerprint")
                .reason,
            CompactAdmissionReason::StorageUnavailable
        );

        tokio::fs::write(dir.path().join("corrupt"), "not json")
            .await
            .expect("write");
        assert_eq!(
            read_admission_row(dir.path(), None, "corrupt")
                .await
                .expect_err("corrupt")
                .reason,
            CompactAdmissionReason::StorageUnavailable
        );
    }
}
