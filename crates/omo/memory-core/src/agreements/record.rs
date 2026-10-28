//! Validation of agreement records emitted by the facts extractor.

use serde_json::{Map, Value};

use super::types::{
    AGREEMENT_TEXT_MAX_CHARS, AgreementExtractionRecord, AgreementScope, AgreementSourceEvent,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgreementRecordValidationError {
    pub message: String,
}

impl std::fmt::Display for AgreementRecordValidationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for AgreementRecordValidationError {}

pub fn parse_agreement_record(
    value: &Value,
    index: usize,
) -> Result<AgreementExtractionRecord, AgreementRecordValidationError> {
    let Some(record) = value.as_object() else {
        return Err(invalid(index, "record must be an object"));
    };
    let date =
        valid_date(record.get("date")).ok_or_else(|| invalid(index, "date must be YYYY-MM-DD"))?;
    let source = record
        .get("source")
        .map(|value| parse_source(value, index))
        .transpose()?;
    let action = record.get("action").and_then(Value::as_str);
    if action == Some("new") {
        assert_keys(
            record,
            &["action", "applies", "text", "date", "source"],
            index,
        )?;
        let text = bounded_text(record.get("text"), index)?
            .ok_or_else(|| invalid(index, "text must be non-empty"))?;
        let applies = match record.get("applies").and_then(Value::as_str) {
            Some("global") => AgreementScope::Global,
            Some("project") => AgreementScope::Project,
            _ => return Err(invalid(index, "applies must be global or project")),
        };
        return Ok(AgreementExtractionRecord::New {
            applies,
            text,
            date,
            source,
        });
    }
    if action != Some("reaffirm") {
        return Err(invalid(index, "action must be new or reaffirm"));
    }
    assert_keys(record, &["action", "id", "date", "text", "source"], index)?;
    let id = record
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| {
            id.len() == 10
                && id
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
        .ok_or_else(|| invalid(index, "id must name an existing agreement"))?;
    let text = match record.get("text") {
        None => None,
        Some(value) => bounded_text(Some(value), index)?,
    };
    Ok(AgreementExtractionRecord::Reaffirm {
        id: id.to_string(),
        date,
        text,
        source,
    })
}

/// Validate only extraction entries, never lead-context messages. Accepts the serialized
/// payload shape so callers can pass either FactsPayload or a versioned wire payload.
pub fn validate_agreement_source(
    record: &AgreementExtractionRecord,
    payload: &impl serde::Serialize,
    index: usize,
) -> Result<(), AgreementRecordValidationError> {
    let payload = serde_json::to_value(payload)
        .map_err(|error| invalid(index, &format!("invalid extraction payload: {error}")))?;
    let source = match record {
        AgreementExtractionRecord::New { source, .. }
        | AgreementExtractionRecord::Reaffirm { source, .. } => source,
    };
    let Some(source) = source else {
        return if payload.get("agreementEventVersion").is_some() {
            Err(invalid(index, "source user-message identity is required"))
        } else {
            Ok(())
        };
    };
    let found = payload
        .get("entries")
        .and_then(Value::as_array)
        .is_some_and(|entries| {
            entries.iter().any(|entry| {
                entry.get("sessionId").and_then(Value::as_str) == Some(source.session_id.as_str())
                    && entry
                        .get("entries")
                        .and_then(Value::as_array)
                        .is_some_and(|messages| {
                            messages.iter().any(|message| {
                                message.get("kind").and_then(Value::as_str) == Some("user")
                                    && message.get("source_message_id").and_then(Value::as_str)
                                        == Some(source.message_id.as_str())
                            })
                        })
            })
        });
    if found {
        Ok(())
    } else {
        Err(invalid(
            index,
            "source must identify a user message in the extraction payload, not lead context",
        ))
    }
}

fn parse_source(
    value: &Value,
    index: usize,
) -> Result<AgreementSourceEvent, AgreementRecordValidationError> {
    let source = value
        .as_object()
        .ok_or_else(|| invalid(index, "source must be an object"))?;
    assert_keys(source, &["sessionId", "messageId"], index)?;
    let nonempty = |key: &str| {
        source
            .get(key)
            .and_then(Value::as_str)
            .filter(|text| !text.trim().is_empty())
    };
    let (Some(session_id), Some(message_id)) = (nonempty("sessionId"), nonempty("messageId"))
    else {
        return Err(invalid(
            index,
            "source requires non-empty sessionId and messageId",
        ));
    };
    Ok(AgreementSourceEvent {
        session_id: session_id.to_string(),
        message_id: message_id.to_string(),
    })
}

pub fn parse_agreement_records_jsonl(
    raw: &str,
) -> Result<Vec<AgreementExtractionRecord>, AgreementRecordValidationError> {
    let mut records = Vec::new();
    for (index, line) in raw.split('\n').enumerate() {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.trim().is_empty() {
            continue;
        }
        let value: Value =
            serde_json::from_str(line).map_err(|_| invalid(index, "line is not valid JSON"))?;
        records.push(parse_agreement_record(&value, index)?);
    }
    Ok(records)
}

/// Trim and collapse every whitespace run to one space.
pub fn clean_agreement_text(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn agreement_text_problem(text: &str) -> Option<String> {
    if text.is_empty() {
        return Some("text must be non-empty".to_string());
    }
    if text.contains("<!--") || text.contains("-->") {
        return Some("text must not contain HTML comment markers".to_string());
    }
    if text.encode_utf16().count() > AGREEMENT_TEXT_MAX_CHARS {
        return Some(format!(
            "text must be at most {AGREEMENT_TEXT_MAX_CHARS} characters"
        ));
    }
    None
}

fn bounded_text(
    value: Option<&Value>,
    index: usize,
) -> Result<Option<String>, AgreementRecordValidationError> {
    let Some(raw) = value
        .and_then(Value::as_str)
        .filter(|raw| !raw.trim().is_empty())
    else {
        return Ok(None);
    };
    let text = clean_agreement_text(raw);
    match agreement_text_problem(&text) {
        Some(problem) => Err(invalid(index, &problem)),
        None => Ok(Some(text)),
    }
}

fn assert_keys(
    value: &Map<String, Value>,
    allowed: &[&str],
    index: usize,
) -> Result<(), AgreementRecordValidationError> {
    match value.keys().find(|key| !allowed.contains(&key.as_str())) {
        Some(extra) => Err(invalid(index, &format!("unexpected field: {extra}"))),
        None => Ok(()),
    }
}

fn valid_date(value: Option<&Value>) -> Option<String> {
    let text = value?.as_str()?;
    let bytes = text.as_bytes();
    let shaped = bytes.len() == 10
        && bytes.iter().enumerate().all(|(index, byte)| {
            if index == 4 || index == 7 {
                *byte == b'-'
            } else {
                byte.is_ascii_digit()
            }
        });
    if !shaped {
        return None;
    }
    let year: u32 = text[0..4].parse().ok()?;
    let month: u32 = text[5..7].parse().ok()?;
    let day: u32 = text[8..10].parse().ok()?;
    let leap = year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400));
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return None,
    };
    (1..=days).contains(&day).then(|| text.to_string())
}

fn invalid(index: usize, message: &str) -> AgreementRecordValidationError {
    AgreementRecordValidationError {
        message: format!("agreement record {}: {message}", index + 1),
    }
}

#[cfg(test)]
#[path = "record_tests.rs"]
mod tests;
