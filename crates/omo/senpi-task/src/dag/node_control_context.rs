//! Shared ownership, journal, and error context for node-control verbs.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::dag::journal::{DagJournal, DagJournalOptions, create_dag_journal};
use crate::dag::manager::DagRunRecordV1;
pub use crate::dag::scheduler::{
    DagNodeControlError, DagNodeControlErrorCode, DagSchedulerOptions,
    apply_dag_scheduler_event,
};
use crate::dag::store::{DagFileStore, DagStoreError};
use crate::dag::types::{DagNodeError, DagNodeId, DagRunId};

pub fn owned_record(
    options: &DagSchedulerOptions,
    run_id: &DagRunId,
) -> Result<DagRunRecordV1, DagNodeControlError> {
    if run_id != &options.initial_record.run_id {
        return Err(DagNodeControlError::new(
            DagNodeControlErrorCode::RunNotOwned,
            format!("dag run \"{run_id}\" is not owned by this scheduler"),
        )
        .with_run_id(run_id.clone()));
    }
    read_record(&options.store, run_id)
        .map_err(|error| {
            DagNodeControlError::new(DagNodeControlErrorCode::InvalidArguments, error.to_string())
                .with_run_id(run_id.clone())
        })?
        .ok_or_else(|| {
            DagNodeControlError::new(
                DagNodeControlErrorCode::RunNotFound,
                format!("unknown dag run \"{run_id}\""),
            )
            .with_run_id(run_id.clone())
        })
}

pub fn read_record(
    store: &DagFileStore,
    run_id: &DagRunId,
) -> Result<Option<DagRunRecordV1>, DagStoreError> {
    store.read_checkpoint(run_id)
}

pub fn current_record(
    options: &DagSchedulerOptions,
    run_id: &DagRunId,
) -> Result<DagRunRecordV1, DagStoreError> {
    Ok(read_record(&options.store, run_id)?.unwrap_or_else(|| options.initial_record.clone()))
}

/// A separate journal recovers the latest durable checkpoint before writing control events.
pub fn control_journal(
    options: &DagSchedulerOptions,
    record: DagRunRecordV1,
    pending_errors: BTreeMap<DagNodeId, DagNodeError>,
) -> Result<Arc<DagJournal<DagRunRecordV1>>, DagStoreError> {
    let store = Arc::clone(&options.store);
    let errors = Arc::new(pending_errors);
    let now = options.now.clone().unwrap_or_else(|| {
        Arc::new(|| i64::try_from(crate::state::system_now_ms()).unwrap_or(i64::MAX))
    });
    create_dag_journal(DagJournalOptions {
        store,
        run_id: record.run_id.clone(),
        initial_checkpoint: record,
        apply_event: Arc::new(move |checkpoint, event| {
            apply_dag_scheduler_event(checkpoint, event, errors.as_ref())
        }),
        subscriber_ring: options.subscriber_ring,
        now: Some(now),
    })
}

pub fn control_error_message(error: &dyn std::fmt::Display) -> String {
    error.to_string()
}
