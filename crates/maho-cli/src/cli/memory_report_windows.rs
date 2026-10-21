use serde_json::{Value, json};
use winsafe::{HPROCESS, PROCESS_MEMORY_COUNTERS_EX, SysResult};

pub(super) fn read_own_footprint() -> Result<Value, String> {
    // GetCurrentProcess returns a pseudo-handle that must not be closed.
    footprint_from_counters(HPROCESS::GetCurrentProcess().GetProcessMemoryInfo())
}

fn footprint_from_counters(counters: SysResult<PROCESS_MEMORY_COUNTERS_EX>) -> Result<Value, String> {
    let counters = counters.map_err(|error| error.to_string())?;
    // PrivateUsage is already in bytes; neither WorkingSetSize nor PagefileUsage
    // is a substitute for the upstream private_usage measurement.
    Ok(json!({"bytes": counters.PrivateUsage, "measure": "private_usage"}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use winsafe::co;

    #[test]
    fn private_usage_preserves_bytes_instead_of_other_memory_counters() {
        // Given
        let mut counters = PROCESS_MEMORY_COUNTERS_EX::default();
        counters.PrivateUsage = 12_345;
        counters.WorkingSetSize = 90_000;
        counters.PagefileUsage = 80_000;
        counters.PeakPagefileUsage = 70_000;
        // When
        let footprint = footprint_from_counters(Ok(counters)).expect("footprint");
        // Then
        assert_eq!(footprint, json!({"bytes": 12_345, "measure": "private_usage"}));
    }

    #[test]
    fn zero_private_usage_does_not_fall_back_to_working_set() {
        // Given
        let mut counters = PROCESS_MEMORY_COUNTERS_EX::default();
        counters.WorkingSetSize = 90_000;
        counters.PagefileUsage = 80_000;
        // When
        let footprint = footprint_from_counters(Ok(counters)).expect("footprint");
        // Then
        assert_eq!(footprint, json!({"bytes": 0, "measure": "private_usage"}));
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn private_usage_preserves_large_integer_bytes() {
        // Given
        let mut counters = PROCESS_MEMORY_COUNTERS_EX::default();
        counters.PrivateUsage = 9_007_199_254_740_993;
        // When
        let footprint = footprint_from_counters(Ok(counters)).expect("footprint");
        // Then
        assert_eq!(footprint["bytes"].as_u64(), Some(9_007_199_254_740_993));
    }

    #[test]
    fn memory_query_failure_propagates_without_fabricating_a_measurement() {
        // Given
        let counters = Err(co::ERROR::ACCESS_DENIED);
        // When
        let footprint = footprint_from_counters(counters);
        // Then
        assert_eq!(footprint, Err(co::ERROR::ACCESS_DENIED.to_string()));
    }
}
