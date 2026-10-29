//! `tools/task/result-details.test.ts`

use pretty_assertions::assert_eq;

use crate::state::{ResolvedModelRecord, ResolvedModelSource};
use crate::tools::task::result_details::record_details;
use crate::tools::task::task_tool_fakes::make_record;
use crate::tools::task::types::TaskToolMode;

#[test]
fn record_with_fallback_attempts_passes_history_to_details() {
    // given
    let fallback_attempts = vec![
        ResolvedModelRecord::new(
            ResolvedModelSource::Category,
            "kimi-coding",
            "kimi-for-coding-highspeed",
        ),
        ResolvedModelRecord::new(
            ResolvedModelSource::Category,
            "quotio-openai",
            "gpt-5.6-luna-fast",
        ),
    ];
    let mut record = make_record();
    record.fallback_attempts = Some(fallback_attempts.clone());

    // when
    let details = record_details(&record, TaskToolMode::Spawn);

    // then
    assert_eq!(details.fallback_attempts, Some(fallback_attempts));
}

#[test]
fn record_with_task_summary_passes_summary_to_details() {
    // given
    let mut record = make_record();
    record.task_summary = Some("Audit the boundary".to_string());

    // when
    let details = record_details(&record, TaskToolMode::Spawn);

    // then
    assert_eq!(details.task_summary, Some("Audit the boundary".to_string()));
}
