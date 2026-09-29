use super::record_store::TaskRecordStore;
use super::types::StoreError;
use crate::state::{
    TaskIdSpaceExhaustedError, TaskRecord, bump_task_id, parse_task_id, sync_task_id_floor,
};

const DEFAULT_MAX_CLAIM_ATTEMPTS: usize = 4096;

/// The one store capability a claim needs; lets callers wrap or fault-inject the save.
pub trait TaskRecordSaver {
    /// Creates the record file, failing with [`StoreError::Collision`] when the id is taken.
    fn save(&self, record: &TaskRecord) -> Result<(), StoreError>;
}

impl TaskRecordSaver for TaskRecordStore {
    fn save(&self, record: &TaskRecord) -> Result<(), StoreError> {
        TaskRecordStore::save(self, record)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameBinding {
    /// The record keeps whatever name the draft carried.
    Preserve,
    /// The record name is re-derived from every bumped id.
    FollowsId,
}

pub struct ClaimOptions<'a> {
    pub max_attempts: usize,
    pub name_binding: NameBinding,
    pub name_available: Option<&'a dyn Fn(&str) -> bool>,
}

impl Default for ClaimOptions<'_> {
    fn default() -> Self {
        Self {
            max_attempts: DEFAULT_MAX_CLAIM_ATTEMPTS,
            name_binding: NameBinding::Preserve,
            name_available: None,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ClaimError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Exhausted(#[from] TaskIdSpaceExhaustedError),
}

fn with_id(candidate: &TaskRecord, next_id: String, name_binding: NameBinding) -> TaskRecord {
    let mut next = candidate.clone();
    if name_binding == NameBinding::FollowsId {
        next.name = Some(next_id.clone());
    }
    next.task_id = next_id;
    next
}

/// Saves `draft`, bumping its id past collisions (and unavailable id-derived names).
pub fn claim_task_record(
    store: &dyn TaskRecordSaver,
    draft: &TaskRecord,
    options: &ClaimOptions<'_>,
) -> Result<TaskRecord, ClaimError> {
    let mut attempts = 0;
    let mut candidate = draft.clone();
    loop {
        if let (NameBinding::FollowsId, Some(name_available)) =
            (options.name_binding, options.name_available)
        {
            while !name_available(candidate.name.as_deref().unwrap_or(&candidate.task_id)) {
                if attempts >= options.max_attempts {
                    return Err(TaskIdSpaceExhaustedError.into());
                }
                attempts += 1;
                let next_id =
                    bump_task_id(parse_task_id(&candidate.task_id).map_err(StoreError::from)?)?;
                candidate = with_id(&candidate, next_id.to_string(), NameBinding::FollowsId);
            }
        }
        if attempts >= options.max_attempts {
            return Err(TaskIdSpaceExhaustedError.into());
        }
        attempts += 1;
        match store.save(&candidate) {
            Ok(()) => {
                sync_task_id_floor(parse_task_id(&candidate.task_id).map_err(StoreError::from)?);
                return Ok(candidate);
            }
            Err(StoreError::Collision { task_id, .. }) if attempts < options.max_attempts => {
                let next_id = bump_task_id(task_id)?;
                candidate = with_id(&candidate, next_id.to_string(), options.name_binding);
            }
            Err(error) => return Err(error.into()),
        }
    }
}
