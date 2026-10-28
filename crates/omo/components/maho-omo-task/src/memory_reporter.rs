//! `memory-reporter.ts`: the task-child layer of senpi's on-demand memory report.
//!
//! Children this session holds resident, and the task records its store keeps parsed. Both are read
//! ONLY when a report is requested, so an idle engine costs nothing for it.

use std::collections::BTreeMap;
use std::sync::Arc;

use maho_ext_api::{ExtensionApi, ExtensionFailure, MemoryReporter};
use senpi_task::store::TaskRecordStoreResidentFigures;

/// Where the task layer's figures come from: the live manager and the live store.
pub trait TaskMemorySource: Send + Sync {
    fn resident_task_ids(&self) -> Vec<String>;
    fn resident_figures(&self) -> TaskRecordStoreResidentFigures;
}

/// The report section name this layer owns.
pub const TASK_MEMORY_REPORT_NAME: &str = "omoTask";

/// `registerTaskMemoryReporter`: register the `omoTask` section, read on demand.
pub fn register_task_memory_reporter(
    api: &ExtensionApi,
    source: Arc<dyn TaskMemorySource>,
) -> Result<(), ExtensionFailure> {
    let reporter: MemoryReporter = Arc::new(move || {
        let mut figures = BTreeMap::new();
        figures.insert("residentChildren".to_string(), source.resident_task_ids().len());
        let resident = source.resident_figures();
        figures.insert("cachedRecords".to_string(), resident.cached_records);
        figures.insert("cachedRecordBytes".to_string(), resident.cached_record_bytes);
        figures.insert("openEventLogs".to_string(), resident.open_event_logs);
        figures
    });
    api.register_memory_reporter(TASK_MEMORY_REPORT_NAME, reporter)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Source {
        ids: Vec<String>,
        figures: TaskRecordStoreResidentFigures,
    }

    impl TaskMemorySource for Source {
        fn resident_task_ids(&self) -> Vec<String> {
            self.ids.clone()
        }
        fn resident_figures(&self) -> TaskRecordStoreResidentFigures {
            self.figures
        }
    }

    #[test]
    fn the_section_reads_the_live_source_only_when_called() {
        let source = Arc::new(Source {
            ids: vec!["st_1".into(), "st_2".into()],
            figures: TaskRecordStoreResidentFigures {
                cached_records: 3,
                cached_record_bytes: 512,
                open_event_logs: 0,
            },
        });
        let reporter = Arc::clone(&source) as Arc<dyn TaskMemorySource>;
        let figures = {
            let mut figures = BTreeMap::new();
            figures.insert(
                "residentChildren".to_string(),
                reporter.resident_task_ids().len(),
            );
            let resident = reporter.resident_figures();
            figures.insert("cachedRecords".to_string(), resident.cached_records);
            figures.insert("cachedRecordBytes".to_string(), resident.cached_record_bytes);
            figures
        };
        assert_eq!(figures.get("residentChildren"), Some(&2));
        assert_eq!(figures.get("cachedRecords"), Some(&3));
        assert_eq!(figures.get("cachedRecordBytes"), Some(&512));
    }
}
