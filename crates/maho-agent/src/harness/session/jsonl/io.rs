//! Port of senpi packages/agent/src/harness/session/jsonl/io.ts.
//!
//! The TS publisher hands the callback a reference to its append function. Rust carries the same
//! contract through owned `Arc` capabilities (`AppendFn`), so the caller's closure can stay
//! `'static` while the observable behavior - stage a sibling `.tmp`, publish by rename, discard the
//! temp file on failure - is unchanged.

use std::sync::Arc;

use crate::harness::context::Context;
use crate::harness::session::commit::{
    CommittedEntryWrite, CommittedListAppendWrite, CommittedListDeleteWrite, CommittedListWrite, CommittedUsageWrite,
    CommittedValueDeleteWrite, CommittedValueSetWrite, CommittedValueWrite, CommittedWrite,
};
use crate::harness::session::session::{SessionError, SessionErrorKind};
use crate::harness::types::{FileFuture, FileResult, FileSystem, TextLineReader};
use serde_json::Value;

use super::codec::{JsonlParsedSessionHeader, parse_jsonl_session_header};
use super::types::JsonlStorageHeader;

/// `fileValue(result, action)`: throw the file error with the action prefix.
pub fn file_value<T>(result: FileResult<T>, action: &str) -> Result<T, SessionError> {
    result.map_err(|error| SessionError::io(format!("{action}: {}", error.message)))
}

pub async fn read_jsonl_header(
    reader: &dyn TextLineReader,
    path: &str,
    context: &Context,
) -> Result<JsonlParsedSessionHeader, SessionError> {
    let line = file_value(
        reader.read_line(context).await,
        &format!("Failed to read JSONL storage {path}"),
    )?;
    match line {
        Some(line) if line.terminated && !line.text.is_empty() => {
            parse_jsonl_session_header(&line.text).map_err(|_| invalid_header(path))
        }
        _ => Err(missing_header(path)),
    }
}

fn missing_header(path: &str) -> SessionError {
    SessionError::new(SessionErrorKind::Io, format!("Invalid JSONL storage {path}: missing header"))
}

fn invalid_header(path: &str) -> SessionError {
    SessionError::new(SessionErrorKind::Io, format!("Invalid JSONL storage {path}: invalid header"))
}

fn invalid_write() -> SessionError {
    SessionError::new(SessionErrorKind::Io, "Invalid JSONL transaction write")
}

fn require_safe_integer(value: Option<&Value>, field: &str, minimum: i64) -> Result<i64, SessionError> {
    match value.and_then(Value::as_i64) {
        Some(number) if number >= minimum => Ok(number),
        _ => Err(SessionError::new(SessionErrorKind::Io, format!("Invalid JSONL {field}"))),
    }
}

pub fn parse_committed_write(value: &Value) -> Result<CommittedWrite, SessionError> {
    if !value.is_object() {
        return Err(invalid_write());
    }
    require_safe_integer(value.get("seq"), "write seq", 1)?;
    match value.get("kind").and_then(Value::as_str) {
        Some("entry") => {
            require_safe_integer(value.get("timestamp"), "entry timestamp", 0)?;
            serde_json::from_value::<CommittedEntryWrite>(value.clone())
                .map(CommittedWrite::Entry)
                .map_err(|_| invalid_write())
        }
        Some("usage") => serde_json::from_value::<CommittedUsageWrite>(value.clone())
            .map(CommittedWrite::Usage)
            .map_err(|_| invalid_write()),
        Some("value") => match value.get("op").and_then(Value::as_str) {
            Some("set") => serde_json::from_value::<CommittedValueSetWrite>(value.clone())
                .map(|write| CommittedWrite::Value(CommittedValueWrite::Set(write)))
                .map_err(|_| invalid_write()),
            Some("delete") => serde_json::from_value::<CommittedValueDeleteWrite>(value.clone())
                .map(|write| CommittedWrite::Value(CommittedValueWrite::Delete(write)))
                .map_err(|_| invalid_write()),
            other => Err(SessionError::new(
                SessionErrorKind::Io,
                format!("Invalid JSONL value operation: {}", other.unwrap_or("undefined")),
            )),
        },
        Some("list") => match value.get("op").and_then(Value::as_str) {
            Some("append") => serde_json::from_value::<CommittedListAppendWrite>(value.clone())
                .map(|write| CommittedWrite::List(CommittedListWrite::Append(write)))
                .map_err(|_| invalid_write()),
            Some("delete") => serde_json::from_value::<CommittedListDeleteWrite>(value.clone())
                .map(|write| CommittedWrite::List(CommittedListWrite::Delete(write)))
                .map_err(|_| invalid_write()),
            other => Err(SessionError::new(
                SessionErrorKind::Io,
                format!("Invalid JSONL list operation: {}", other.unwrap_or("undefined")),
            )),
        },
        other => Err(SessionError::new(
            SessionErrorKind::Io,
            format!("Invalid JSONL write kind: {}", other.unwrap_or("undefined")),
        )),
    }
}

pub fn parse_jsonl_transaction(line: &str) -> Result<Vec<CommittedWrite>, SessionError> {
    let Ok(value) = serde_json::from_str::<Value>(line) else {
        return Err(SessionError::new(
            SessionErrorKind::Io,
            "Invalid JSONL transaction: not valid JSON",
        ));
    };
    let values = match value {
        Value::Array(values) => values,
        other => vec![other],
    };
    values.iter().map(parse_committed_write).collect()
}

pub fn serialize_jsonl_transaction(writes: &[CommittedWrite]) -> String {
    let value = if writes.len() == 1 {
        serde_json::to_value(&writes[0]).unwrap_or(Value::Null)
    } else {
        serde_json::to_value(writes).unwrap_or(Value::Null)
    };
    value.to_string()
}

/// Append capability handed to the publication callback.
pub type AppendFn = Arc<dyn Fn(String) -> FileFuture<'static, Result<(), SessionError>> + Send + Sync>;

/// Transaction append capability handed to the JSONL publication callback.
pub type TxAppendFn =
    Arc<dyn Fn(Vec<CommittedWrite>) -> FileFuture<'static, Result<(), SessionError>> + Send + Sync>;

/// Content generator run between the temp-file stage and the publishing rename.
pub type PublishContentFn =
    Arc<dyn Fn(AppendFn) -> FileFuture<'static, Result<(), SessionError>> + Send + Sync>;

/// Publish only after the callback succeeds; it must await each append before returning.
pub async fn publish_file_atomically(
    file_system: Arc<dyn FileSystem>,
    destination_path: String,
    context: &Context,
    write_content: PublishContentFn,
) -> Result<(), SessionError> {
    let context = context.clone();
    let temp_path = format!("{destination_path}.tmp");
    let outcome = async {
        file_value(
            file_system
                .write_file(&temp_path, b"", &context)
                .await,
            &format!("Failed to stage JSONL storage {destination_path}"),
        )?;
        let append: AppendFn = {
            let file_system = file_system.clone();
            let temp_path = temp_path.clone();
            let context = context.clone();
            let destination_path = destination_path.clone();
            Arc::new(move |content: String| {
                let file_system = file_system.clone();
                let temp_path = temp_path.clone();
                let context = context.clone();
                let destination_path = destination_path.clone();
                Box::pin(async move {
                    file_value(
                        file_system
                            .append_file(&temp_path, content.as_bytes(), &context)
                            .await,
                        &format!("Failed to append JSONL storage {destination_path}"),
                    )?;
                    Ok(())
                })
            })
        };
        write_content(append).await?;
        file_value(
            file_system.rename_file(&temp_path, &destination_path, &context).await,
            &format!("Failed to publish JSONL storage {destination_path}"),
        )?;
        Ok(())
    }
    .await;
    if outcome.is_err() {
        let _ = file_system.remove(&temp_path, None, Some(true), &context).await;
    }
    outcome
}

/// Stream a header and complete transactions through the shared atomic publisher.
pub async fn publish_jsonl(
    file_system: Arc<dyn FileSystem>,
    destination_path: String,
    header: &JsonlStorageHeader,
    context: &Context,
    write_transactions: Arc<dyn Fn(TxAppendFn) -> FileFuture<'static, Result<(), SessionError>> + Send + Sync>,
) -> Result<(), SessionError> {
    let header_line = format!("{}\n", serde_json::to_string(header).unwrap_or_default());
    publish_file_atomically(
        file_system,
        destination_path,
        context,
        Arc::new(move |append: AppendFn| {
            let header_line = header_line.clone();
            let write_transactions = write_transactions.clone();
            Box::pin(async move {
                append(header_line).await?;
                write_transactions(Arc::new(move |writes: Vec<CommittedWrite>| {
                    let line = format!("{}\n", serialize_jsonl_transaction(&writes));
                    let append = append.clone();
                    Box::pin(async move { append(line).await })
                }))
                .await
            })
        }),
    )
    .await
}
