//! Facts extraction JSONL parsing, validation, and batch application.

use serde::{Deserialize, Serialize};

use crate::git::{GitCommitAuthor, GitError, GitMemoryRepo};

use super::mutation_plan::{
    FactsApplyRecovery, MutationPlanError, facts_records_hash, plan_facts_mutation,
};
use super::person_routing::{FactsAliasTie, FactsPeopleRouting};
use super::recovery::{FactsRecoveryError, FactsRecoveryResult, apply_facts_recovery};
use super::schema::FactsQueueEntry;

/// A person identity reference with primary name and aliases.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactsPersonReference {
    pub name: String,
    pub aliases: Vec<String>,
}

/// A parsed and validated fact extraction record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "scope", rename_all = "lowercase")]
pub enum FactsExtractionRecord {
    Project {
        text: String,
        date: String,
    },
    Person {
        person: FactsPersonReference,
        text: String,
        date: String,
    },
}

impl FactsExtractionRecord {
    /// Return the fact text.
    pub fn text(&self) -> &str {
        match self {
            Self::Project { text, .. } => text,
            Self::Person { text, .. } => text,
        }
    }

    /// Return the fact date (YYYY-MM-DD).
    pub fn date(&self) -> &str {
        match self {
            Self::Project { date, .. } => date,
            Self::Person { date, .. } => date,
        }
    }
}

/// A batch of extracted facts with a unique UUID v4 identifier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactsBatch {
    #[serde(rename = "batchId")]
    pub batch_id: String,
    pub records: Vec<FactsExtractionRecord>,
}

/// A known person record in the facts index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactsKnownPerson {
    pub slug: String,
    #[serde(rename = "displayName")]
    pub display_name: String,
    pub aliases: Vec<String>,
}

/// Primary human configuration for routing human-scoped facts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrimaryHuman {
    pub slug: String,
    pub aliases: Vec<String>,
}

/// Facts prompt compilation payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactsPayload {
    pub version: u32,
    pub identity: String,
    pub today: String,
    pub entries: Vec<FactsQueueEntry>,
    #[serde(rename = "knownPeople")]
    pub known_people: Vec<FactsKnownPerson>,
    #[serde(rename = "primaryHuman")]
    pub primary_human: PrimaryHuman,
}

/// Outcome of applying a facts extraction batch to the repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyFactsBatchResult {
    Committed {
        sha: String,
        affected_paths: Vec<String>,
    },
    NoFacts {
        affected_paths: Vec<String>,
    },
    ParentDirty,
}

impl ApplyFactsBatchResult {
    /// Outcome status string matching TypeScript wire format.
    pub fn outcome(&self) -> &str {
        match self {
            Self::Committed { .. } => "committed",
            Self::NoFacts { .. } => "no_facts",
            Self::ParentDirty => "parent_dirty",
        }
    }

    /// Commit SHA if committed.
    pub fn sha(&self) -> Option<&str> {
        match self {
            Self::Committed { sha, .. } => Some(sha),
            _ => None,
        }
    }

    /// Affected repository paths.
    pub fn affected_paths(&self) -> &[String] {
        match self {
            Self::Committed { affected_paths, .. } => affected_paths,
            Self::NoFacts { affected_paths, .. } => affected_paths,
            Self::ParentDirty => &[],
        }
    }
}

/// Callback that durably publishes the recovery envelope before mutation starts.
pub type PublishRecovery<'a> =
    Box<dyn FnMut(&FactsApplyRecovery) -> Result<(), FactsExtractionError> + 'a>;

/// Options controlling facts batch application.
#[derive(Default)]
pub struct ApplyFactsBatchOptions<'a> {
    pub people: Option<FactsPeopleRouting>,
    pub on_alias_tie: Option<&'a mut dyn FnMut(&FactsAliasTie)>,
    pub publish_recovery: Option<PublishRecovery<'a>>,
}

/// Validation error encountered while parsing extraction JSONL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactsExtractionValidationError {
    pub message: String,
}

impl std::fmt::Display for FactsExtractionValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for FactsExtractionValidationError {}

/// Errors returned by facts extraction operations.
#[derive(Debug)]
pub enum FactsExtractionError {
    Validation(FactsExtractionValidationError),
    InvalidBatchId(String),
    InvalidRecovery(String),
    Plan(MutationPlanError),
    Recovery(FactsRecoveryError),
    Git(GitError),
    Io(std::io::Error),
    Json(serde_json::Error),
}

impl std::fmt::Display for FactsExtractionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Validation(err) => write!(f, "{err}"),
            Self::InvalidBatchId(msg) => write!(f, "invalid batch id: {msg}"),
            Self::InvalidRecovery(msg) => write!(f, "invalid recovery: {msg}"),
            Self::Plan(err) => write!(f, "facts plan error: {err}"),
            Self::Recovery(err) => write!(f, "facts recovery error: {err}"),
            Self::Git(err) => write!(f, "git error: {err}"),
            Self::Io(err) => write!(f, "io error: {err}"),
            Self::Json(err) => write!(f, "json error: {err}"),
        }
    }
}

impl std::error::Error for FactsExtractionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Validation(err) => Some(err),
            Self::Plan(err) => Some(err),
            Self::Recovery(err) => Some(err),
            Self::Git(err) => Some(err),
            Self::Io(err) => Some(err),
            Self::Json(err) => Some(err),
            Self::InvalidBatchId(_) | Self::InvalidRecovery(_) => None,
        }
    }
}

impl From<FactsExtractionValidationError> for FactsExtractionError {
    fn from(err: FactsExtractionValidationError) -> Self {
        Self::Validation(err)
    }
}

impl From<MutationPlanError> for FactsExtractionError {
    fn from(err: MutationPlanError) -> Self {
        Self::Plan(err)
    }
}

impl From<FactsRecoveryError> for FactsExtractionError {
    fn from(err: FactsRecoveryError) -> Self {
        Self::Recovery(err)
    }
}

impl From<GitError> for FactsExtractionError {
    fn from(err: GitError) -> Self {
        Self::Git(err)
    }
}

impl From<std::io::Error> for FactsExtractionError {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err)
    }
}

impl From<serde_json::Error> for FactsExtractionError {
    fn from(err: serde_json::Error) -> Self {
        Self::Json(err)
    }
}

/// Parse and validate JSONL extraction records.
pub fn parse_facts_extraction_jsonl(
    raw: &str,
) -> Result<Vec<FactsExtractionRecord>, FactsExtractionValidationError> {
    let mut records = Vec::new();
    for (index, line) in raw.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let value: serde_json::Value =
            serde_json::from_str(trimmed).map_err(|_| invalid(index, "line is not valid JSON"))?;
        records.push(parse_record(&value, index)?);
    }
    Ok(records)
}

/// Apply a batch of facts to the memory repository with path-state protection.
pub fn apply_facts_batch(
    repo: &GitMemoryRepo,
    batch: FactsBatch,
    author: &GitCommitAuthor,
    options: ApplyFactsBatchOptions<'_>,
) -> Result<ApplyFactsBatchResult, FactsExtractionError> {
    validate_facts_batch_id(&batch.batch_id)?;
    if batch.records.is_empty() {
        return Ok(ApplyFactsBatchResult::NoFacts {
            affected_paths: Vec::new(),
        });
    }
    let recovery =
        match plan_facts_mutation(repo, &batch, options.people.as_ref(), options.on_alias_tie) {
            Ok(rec) => rec,
            Err(MutationPlanError::ParentDirty(_)) => {
                return Ok(ApplyFactsBatchResult::ParentDirty);
            }
            Err(err) => return Err(FactsExtractionError::Plan(err)),
        };
    if let Some(mut publish) = options.publish_recovery {
        publish(&recovery)?;
    }
    let recovery_result = apply_facts_recovery(repo, &recovery, batch.records.len(), author)?;
    match recovery_result {
        FactsRecoveryResult::Committed {
            sha,
            affected_paths,
        } => Ok(ApplyFactsBatchResult::Committed {
            sha,
            affected_paths,
        }),
        FactsRecoveryResult::ParentDirty { .. } => Ok(ApplyFactsBatchResult::ParentDirty),
    }
}

/// Validate that a recovery envelope matches its originating batch.
pub fn validate_facts_recovery(
    recovery: &FactsApplyRecovery,
    batch: &FactsBatch,
) -> Result<(), FactsExtractionError> {
    validate_facts_batch_id(&batch.batch_id)?;
    if recovery.version != 1
        || recovery.batch_id != batch.batch_id
        || recovery.records_hash != facts_records_hash(&batch.records)
        || recovery.paths.windows(2).any(|w| w[0].path >= w[1].path)
    {
        return Err(FactsExtractionError::InvalidRecovery(
            "Invalid facts applyRecovery envelope".to_string(),
        ));
    }
    Ok(())
}

fn parse_record(
    value: &serde_json::Value,
    index: usize,
) -> Result<FactsExtractionRecord, FactsExtractionValidationError> {
    let Some(obj) = value.as_object() else {
        return Err(invalid(index, "record must be an object"));
    };
    let text =
        non_empty_str(obj.get("text")).ok_or_else(|| invalid(index, "text must be non-empty"))?;
    let date_str = obj
        .get("date")
        .and_then(|v| v.as_str())
        .ok_or_else(|| invalid(index, "date must be YYYY-MM-DD"))?;
    let date = valid_date(date_str).ok_or_else(|| invalid(index, "date must be YYYY-MM-DD"))?;

    let scope = obj
        .get("scope")
        .and_then(|v| v.as_str())
        .ok_or_else(|| invalid(index, "scope must be person or project"))?;

    if scope == "project" {
        if obj.contains_key("person") {
            return Err(invalid(index, "project record must not carry person"));
        }
        assert_allowed_keys(obj, &["scope", "text", "date"], index)?;
        return Ok(FactsExtractionRecord::Project { text, date });
    }

    if scope != "person" {
        return Err(invalid(index, "scope must be person or project"));
    }

    if !obj.contains_key("person") {
        return Err(invalid(index, "person record requires person"));
    }
    assert_allowed_keys(obj, &["scope", "person", "text", "date"], index)?;

    let Some(person_obj) = obj.get("person").and_then(|v| v.as_object()) else {
        return Err(invalid(index, "person must be an object"));
    };
    assert_allowed_keys(person_obj, &["name", "aliases"], index)?;

    let name = non_empty_str(person_obj.get("name"))
        .ok_or_else(|| invalid(index, "person requires name and aliases"))?;
    let aliases_val = person_obj
        .get("aliases")
        .and_then(|v| v.as_array())
        .ok_or_else(|| invalid(index, "person requires name and aliases"))?;

    let mut aliases = Vec::with_capacity(aliases_val.len());
    for alias_item in aliases_val {
        let alias = non_empty_str(Some(alias_item))
            .ok_or_else(|| invalid(index, "person aliases must be non-empty strings"))?;
        aliases.push(alias);
    }

    Ok(FactsExtractionRecord::Person {
        person: FactsPersonReference { name, aliases },
        text,
        date,
    })
}

fn assert_allowed_keys(
    obj: &serde_json::Map<String, serde_json::Value>,
    allowed: &[&str],
    index: usize,
) -> Result<(), FactsExtractionValidationError> {
    for key in obj.keys() {
        if !allowed.contains(&key.as_str()) {
            return Err(invalid(index, format!("unexpected field: {key}")));
        }
    }
    Ok(())
}

fn validate_facts_batch_id(batch_id: &str) -> Result<(), FactsExtractionError> {
    if batch_id.len() != 36 {
        return Err(FactsExtractionError::InvalidBatchId(
            "facts batchId must be a UUID v4".to_string(),
        ));
    }
    let b = batch_id.as_bytes();
    if b[8] != b'-' || b[13] != b'-' || b[18] != b'-' || b[23] != b'-' {
        return Err(FactsExtractionError::InvalidBatchId(
            "facts batchId must be a UUID v4".to_string(),
        ));
    }
    for (i, &byte) in b.iter().enumerate() {
        if i == 8 || i == 13 || i == 18 || i == 23 {
            continue;
        }
        if !byte.is_ascii_hexdigit() {
            return Err(FactsExtractionError::InvalidBatchId(
                "facts batchId must be a UUID v4".to_string(),
            ));
        }
    }
    if b[14] != b'4' {
        return Err(FactsExtractionError::InvalidBatchId(
            "facts batchId must be a UUID v4".to_string(),
        ));
    }
    if !matches!(b[19], b'8' | b'9' | b'a' | b'b' | b'A' | b'B') {
        return Err(FactsExtractionError::InvalidBatchId(
            "facts batchId must be a UUID v4".to_string(),
        ));
    }
    Ok(())
}

fn valid_date(value: &str) -> Option<String> {
    if value.len() != 10 {
        return None;
    }
    let b = value.as_bytes();
    if b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let month: u32 = value[5..7].parse().ok()?;
    let day: u32 = value[8..10].parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let iso = format!("{value}T00:00:00.000Z");
    let millis = crate::support::time::parse_rfc3339(&iso)?;
    let roundtrip = crate::support::time::format_rfc3339_millis(millis);
    if &roundtrip[..10] == value {
        Some(value.to_string())
    } else {
        None
    }
}

fn non_empty_str(value: Option<&serde_json::Value>) -> Option<String> {
    let s = value.and_then(|v| v.as_str())?;
    let trimmed = s.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn invalid(index: usize, message: impl std::fmt::Display) -> FactsExtractionValidationError {
    FactsExtractionValidationError {
        message: format!("facts extraction line {}: {message}", index + 1),
    }
}

#[cfg(test)]
#[path = "extraction_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "extraction_apply_tests.rs"]
mod apply_tests;
