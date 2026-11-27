//! senpi `modes/rpc/compact-recovery-base.ts`: the loaded transcript base as the page route
//! streams it (S11c, RULING-70 (1) B, RULING-71, RULING-72).

use crate::accepted_transcript::TranscriptRoot;
use crate::compact_recovery_base_index::{BASE_ROW_TABLE_BYTES, BaseRowTable, LineSpan, WalkMode};
use crate::compact_recovery_bytes::{
    CancelSignal, FrozenBytes, RecoveryBytesError, ValidCheck, assert_range, stored_bytes,
};
use crate::compact_recovery_ids::{
    BaseRef, ContentDigest, ContentRef, base_ref, parse_content_digest,
};
use crate::compact_recovery_manifest_shape::entry_family;
use futures::future::BoxFuture;
use maho_core::session_file_identity::BaseContentHash;
use maho_core::session_transcript_journal::{PersistedBase, PersistedBaseError, fingerprint_line};
use serde_json::{Value, json};

fn parse_session_entry_line(line: &str) -> Option<Value> {
    if line.trim().is_empty() {
        return None;
    }
    serde_json::from_str::<Value>(line).ok()
}
use sha2::{Digest, Sha256};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::io::AsyncReadExt;

const READ_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone)]
struct Line {
    start: usize,
    end: usize,
    text: String,
}

/// The base this cut's reserved ref names; another cut's ref, an unavailable transcript or no base is `Range`.
pub fn base_stream<'a>(
    transcript: &'a TranscriptRoot,
    catalog_root: &ContentRef,
    ref_: &BaseRef,
) -> Result<&'a PersistedBase, RecoveryBytesError> {
    if ref_ != &base_ref(catalog_root) || transcript.unavailable.is_some() {
        return Err(RecoveryBytesError::Range);
    }
    let Some(base) = transcript.base.as_ref() else {
        return Err(RecoveryBytesError::Range);
    };
    if base.count == 0 {
        return Err(RecoveryBytesError::Range);
    }
    Ok(base)
}

async fn open_identified(
    base: &PersistedBase,
    end: Option<usize>,
) -> Result<tokio::fs::File, RecoveryBytesError> {
    let file = tokio::fs::File::open(&base.file_identity.path)
        .await
        .map_err(|_| RecoveryBytesError::Base(PersistedBaseError::BaseFileMissing))?;
    let std_file = file
        .try_clone()
        .await
        .map_err(|_| RecoveryBytesError::Base(PersistedBaseError::BaseFileMissing))?
        .into_std()
        .await;
    if !maho_core::session_file_identity::same_file(&std_file, &base.file_identity) {
        return Err(RecoveryBytesError::Base(
            PersistedBaseError::BaseFileReplaced,
        ));
    }
    let meta = std_file
        .metadata()
        .map_err(|_| RecoveryBytesError::Base(PersistedBaseError::BaseFileMissing))?;
    if let Some(required_end) = end
        && meta.len() < required_end as u64
    {
        return Err(RecoveryBytesError::Base(
            PersistedBaseError::BaseFileTruncated,
        ));
    }
    Ok(file)
}

async fn read_lines_from(
    file: &mut tokio::fs::File,
    from: usize,
    mut on_line: impl FnMut(Line) -> Result<bool, RecoveryBytesError> + Send,
    check: &(dyn Fn() -> Result<(), RecoveryBytesError> + Sync),
) -> Result<(), RecoveryBytesError> {
    use tokio::io::AsyncSeekExt;
    file.seek(std::io::SeekFrom::Start(from as u64))
        .await
        .map_err(|e| RecoveryBytesError::Io(e.to_string()))?;
    let mut buffer = vec![0u8; READ_BYTES];
    let mut carried: Vec<u8> = Vec::new();
    let mut start = from;
    let mut position = from;
    loop {
        let n = file
            .read(&mut buffer)
            .await
            .map_err(|e| RecoveryBytesError::Io(e.to_string()))?;
        check()?;
        if n == 0 {
            break;
        }
        let chunk = &buffer[..n];
        let mut at = 0usize;
        while let Some(rel) = chunk[at..].iter().position(|&b| b == b'\n') {
            let newline = at + rel;
            let end = position + newline;
            let mut line_bytes = std::mem::take(&mut carried);
            line_bytes.extend_from_slice(&chunk[at..newline]);
            let text = String::from_utf8_lossy(&line_bytes).into_owned();
            if on_line(Line { start, end, text })? {
                return Ok(());
            }
            at = newline + 1;
            start = end + 1;
        }
        if at < n {
            carried.extend_from_slice(&chunk[at..]);
        }
        position += n;
    }
    if !carried.is_empty() {
        let text = String::from_utf8_lossy(&carried).into_owned();
        let _ = on_line(Line {
            start,
            end: position,
            text,
        })?;
    }
    Ok(())
}

fn entry_field_str(entry: &Value, key: &str) -> String {
    match entry.get(key) {
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
        None => "undefined".to_owned(),
    }
}

fn stamped_row(index: usize, entry: &Value) -> Vec<u8> {
    let kind = entry_field_str(entry, "type");
    let parent_id = entry
        .get("parentId")
        .and_then(Value::as_str)
        .map(Value::from)
        .unwrap_or(Value::Null);
    let line = format!(
        "{}\n",
        json!({
            "index": index,
            "id": entry_field_str(entry, "id"),
            "source": "file",
            "parentId": parent_id,
            "kind": kind,
            "family": entry_family(&kind).as_str(),
        })
    );
    stored_bytes(&line)
}

pub struct BaseBytes {
    length: usize,
    digest: ContentDigest,
    rows_per_anchor: usize,
    table_bytes: usize,
    peak_resident: AtomicUsize,
    peak_range_rows: AtomicUsize,
    resident: AtomicUsize,
    base: PersistedBase,
    rows: tokio::sync::Mutex<BaseRowTable>,
}

impl BaseBytes {
    pub fn rows_per_anchor(&self) -> usize {
        self.rows_per_anchor
    }

    pub fn table_bytes(&self) -> usize {
        self.table_bytes
    }

    pub fn peak_range_rows(&self) -> usize {
        self.peak_range_rows.load(Ordering::SeqCst)
    }
}

impl FrozenBytes for BaseBytes {
    fn length(&self) -> usize {
        self.length
    }

    fn digest(&self) -> &ContentDigest {
        &self.digest
    }

    fn peak_resident_leaves(&self) -> usize {
        self.peak_resident.load(Ordering::SeqCst)
    }

    fn range<'a>(
        &'a self,
        offset: usize,
        length: usize,
        valid: Option<ValidCheck<'a>>,
    ) -> BoxFuture<'a, Result<Vec<u8>, RecoveryBytesError>> {
        Box::pin(async move {
            assert_range(offset, length, self.length)?;
            let mut out = vec![0u8; length];
            let end = offset + length;
            let file_end = self.rows.lock().await.file_end;
            let mut file = open_identified(&self.base, Some(file_end)).await?;
            let mut table = self.rows.lock().await;
            let mut walk = table.walk(offset, WalkMode::Verify);
            let file_start = walk.file_start;
            let mut rows_seen = 0usize;
            let mut finished = false;
            let check = || -> Result<(), RecoveryBytesError> {
                if let Some(v) = valid {
                    v()?;
                }
                Ok(())
            };
            read_lines_from(
                &mut file,
                file_start,
                |line| {
                    let trimmed = line.text.trim_end_matches('\r');
                    let Some(entry) = parse_session_entry_line(trimmed) else {
                        return Ok(false);
                    };
                    let now = self.resident.fetch_add(1, Ordering::SeqCst) + 1;
                    self.peak_resident.fetch_max(now, Ordering::SeqCst);
                    let res = (|| {
                        let from = walk.at;
                        let row = stamped_row(walk.row, &entry);
                        let closed = walk.add(
                            LineSpan {
                                start: line.start,
                                end: line.end,
                            },
                            &row,
                        )?;
                        let first = offset.max(from);
                        let last = end.min(from + row.len());
                        if last > first {
                            out[first - offset..last - offset]
                                .copy_from_slice(&row[first - from..last - from]);
                        }
                        rows_seen += 1;
                        self.peak_range_rows.fetch_max(rows_seen, Ordering::SeqCst);
                        if closed && walk.at >= end {
                            finished = true;
                            return Ok(true);
                        }
                        Ok(false)
                    })();
                    self.resident.fetch_sub(1, Ordering::SeqCst);
                    res
                },
                &check,
            )
            .await?;
            if !finished {
                return Err(RecoveryBytesError::Base(
                    PersistedBaseError::BaseFileTruncated,
                ));
            }
            Ok(out)
        })
    }
}

/// Opens the base's stamped rows after one verifying scan of the file.
pub async fn open_base_bytes(
    base: &PersistedBase,
    valid: ValidCheck<'_>,
    signal: Option<&CancelSignal>,
    budget: Option<usize>,
    expected_content_fingerprint: Option<&str>,
) -> Result<Arc<BaseBytes>, RecoveryBytesError> {
    let budget = budget.unwrap_or(BASE_ROW_TABLE_BYTES);
    let check = || -> Result<(), RecoveryBytesError> {
        if signal.is_some_and(CancelSignal::is_aborted) {
            return Err(RecoveryBytesError::Aborted);
        }
        valid()
    };
    let mut file = open_identified(base, None).await?;
    let mut rows = BaseRowTable::new(base.count, budget).map_err(RecoveryBytesError::Io)?;
    let mut fingerprint = Sha256::new();
    let mut content = BaseContentHash::new();
    {
        let mut walk = rows.walk(0, WalkMode::Record);
        let count = base.count;
        read_lines_from(
            &mut file,
            0,
            |line| {
                let trimmed = line.text.trim_end_matches('\r');
                let Some(entry) = parse_session_entry_line(trimmed) else {
                    return Ok(false);
                };
                fingerprint.update(fingerprint_line(&entry).as_bytes());
                content.line(trimmed);
                let row = stamped_row(walk.row, &entry);
                walk.add(
                    LineSpan {
                        start: line.start,
                        end: line.end,
                    },
                    &row,
                )?;
                Ok(walk.row == count)
            },
            &check,
        )
        .await?;
        if walk.row < base.count {
            return Err(RecoveryBytesError::Base(
                PersistedBaseError::BaseFileTruncated,
            ));
        }
    }
    let digest_hex = hex::encode(fingerprint.finalize());
    let Ok(digest) = parse_content_digest(&digest_hex) else {
        return Err(RecoveryBytesError::Base(
            PersistedBaseError::BaseFingerprintMismatch,
        ));
    };
    if digest.as_str() != base.id_fingerprint {
        return Err(RecoveryBytesError::Base(
            PersistedBaseError::BaseFingerprintMismatch,
        ));
    }
    if let Some(expected_content) = expected_content_fingerprint
        && content.digest() != expected_content
    {
        return Err(RecoveryBytesError::Base(
            PersistedBaseError::BaseFingerprintMismatch,
        ));
    }
    let length = rows.length();
    let rows_per_anchor = rows.rows_per_anchor;
    let table_bytes = rows.table_bytes();
    Ok(Arc::new(BaseBytes {
        length,
        digest,
        rows_per_anchor,
        table_bytes,
        peak_resident: AtomicUsize::new(0),
        peak_range_rows: AtomicUsize::new(0),
        resident: AtomicUsize::new(0),
        base: base.clone(),
        rows: tokio::sync::Mutex::new(rows),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compact_recovery_bytes::produce_page;
    use maho_core::session_file_identity::capture_file_identity;
    use maho_core::session_transcript_journal::capture_transcript_base;
    use std::fs::File;

    fn ok_valid() -> Result<(), RecoveryBytesError> {
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn opens_and_streams_base_rows_and_detects_truncation_or_replacement() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("session.jsonl");
        let entries = vec![
            json!({"type":"session","id":"s1"}),
            json!({"type":"message","id":"m1","parentId":"s1"}),
            json!({"type":"custom_ext","id":"x1","parentId":"m1"}),
        ];
        let raw: String = entries.iter().map(|e| format!("{e}\n")).collect();
        std::fs::write(&path, &raw).expect("write");
        let file = File::open(&path).expect("open");
        let identity = capture_file_identity(&file, &path.to_string_lossy()).expect("id");
        let captured = capture_transcript_base(&entries);
        let mut content_hash = BaseContentHash::new();
        for e in &entries {
            content_hash.line(&e.to_string());
        }
        let content_fp = content_hash.digest();
        let base = PersistedBase {
            file_identity: identity,
            count: captured.count,
            id_fingerprint: captured.id_fingerprint,
        };

        let bytes = open_base_bytes(&base, &ok_valid, None, None, Some(&content_fp))
            .await
            .expect("open base");
        let page = produce_page(bytes.as_ref(), 0, Some(&ok_valid))
            .await
            .expect("page");
        assert!(page.complete);
        let decoded: String = page
            .bytes
            .chunks_exact(2)
            .map(|c| char::from_u32(u32::from(u16::from_le_bytes([c[0], c[1]]))).expect("utf16"))
            .collect();
        let lines: Vec<&str> = decoded.trim_end().split('\n').collect();
        assert_eq!(lines.len(), 3);
        let row2: Value = serde_json::from_str(lines[2]).expect("json");
        assert_eq!(row2["family"], "opaque");

        // Rewrite file via rename -> BaseFileReplaced
        let replacement = dir.path().join("next");
        std::fs::write(&replacement, &raw).expect("write next");
        std::fs::rename(&replacement, &path).expect("rename");
        let err = bytes.range(0, 4, None).await.expect_err("replaced");
        assert_eq!(
            err,
            RecoveryBytesError::Base(PersistedBaseError::BaseFileReplaced)
        );
    }

    #[test]
    fn base_stream_rejects_foreign_ref_or_unavailable_transcript() {
        let root = crate::compact_recovery_ids::catalog_root_ref(1);
        let other = crate::compact_recovery_ids::catalog_root_ref(2);
        let other_base = base_ref(&other);
        let mut transcript = crate::accepted_transcript::AcceptedTranscript::default().freeze();
        assert_eq!(
            base_stream(&transcript, &root, &other_base),
            Err(RecoveryBytesError::Range)
        );
        transcript.unavailable = Some(
            maho_core::session_transcript_journal::TranscriptUnavailableReason::TranscriptTailTrimmed,
        );
        assert_eq!(
            base_stream(&transcript, &root, &base_ref(&root)),
            Err(RecoveryBytesError::Range)
        );
    }
}
