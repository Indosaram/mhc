use super::types::TaskRecord;

/// Pre-field records retain one fixed run key even when their notification epoch moves.
pub const LEGACY_RUN_START_EPOCH: i64 = -1;

pub(crate) fn is_legacy_run_start_epoch(epoch: &i64) -> bool {
    *epoch == LEGACY_RUN_START_EPOCH
}

/// No record means no run; old persisted records use the fixed legacy key.
pub fn current_run_start_epoch(record: Option<&TaskRecord>) -> Option<i64> {
    record.map(|record| record.run_start_epoch)
}
