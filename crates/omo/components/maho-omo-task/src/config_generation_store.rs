//! Port of omo `packages/omo-senpi/src/components/task/config-generation-store.ts`.

use senpi_task::state::TaskRecord;
use senpi_task::store::{StoreError, TaskRecordStore};

pub struct ConfigGenerationStampingStore {
    backing: TaskRecordStore,
    current_generation: Box<dyn Fn() -> Option<u64> + Send + Sync>,
}

impl ConfigGenerationStampingStore {
    pub fn new(
        backing: TaskRecordStore,
        current_generation: impl Fn() -> Option<u64> + Send + Sync + 'static,
    ) -> Self {
        Self {
            backing,
            current_generation: Box::new(current_generation),
        }
    }

    fn stamp(&self, record: &TaskRecord) -> Result<Option<u64>, StoreError> {
        if record.config_generation.is_some() {
            return Ok(record.config_generation);
        }
        let persisted = self.backing.load(&record.task_id)?;
        Ok(match persisted {
            None => (self.current_generation)(),
            Some(persisted) => persisted.config_generation,
        })
    }

    pub fn save(&self, record: &TaskRecord) -> Result<(), StoreError> {
        let stamped = self.stamp(record)?;
        self.backing.save(&with_generation(record, stamped))
    }

    pub fn replace(&self, record: &TaskRecord) -> Result<(), StoreError> {
        let stamped = self.stamp(record)?;
        self.backing.replace(&with_generation(record, stamped))
    }

    pub fn load(&self, task_id: &str) -> Result<Option<TaskRecord>, StoreError> {
        self.backing.load(task_id)
    }
}

fn with_generation(record: &TaskRecord, generation: Option<u64>) -> TaskRecord {
    match generation {
        Some(generation) => TaskRecord {
            config_generation: Some(generation),
            ..record.clone()
        },
        None => record.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, TaskRecordStore) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = TaskRecordStore::new(&senpi_task::store::StateDirConfig {
            project_dir: dir.path().to_path_buf(),
            task_state_dir: Some(dir.path().join("tasks")),
        });
        (dir, store)
    }

    fn record(id: &str) -> TaskRecord {
        let input = senpi_task::state::TaskRecordInput {
            execution_mode: "in-process".into(),
            ..Default::default()
        };
        let mut record =
            senpi_task::state::create_task_record(input, Some(1_700_000_000_000)).expect("record");
        record.task_id = id.to_string();
        record
    }

    const A: &str = "st_0000000a";
    const B: &str = "st_0000000b";

    #[test]
    fn a_fresh_record_takes_the_current_generation() {
        let (_dir, backing) = store();
        let store = ConfigGenerationStampingStore::new(backing, || Some(7));
        store.save(&record(A)).expect("save");
        assert_eq!(
            store.load(A).expect("load").unwrap().config_generation,
            Some(7)
        );
        store.save(&record(B)).expect("save");
        assert_eq!(
            store.load(B).expect("load").unwrap().config_generation,
            Some(7)
        );
    }

    #[test]
    fn a_replaced_record_keeps_the_persisted_generation() {
        let (_dir, backing) = store();
        let current = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(1));
        let gate = std::sync::Arc::clone(&current);
        let store = ConfigGenerationStampingStore::new(backing, move || {
            Some(gate.load(std::sync::atomic::Ordering::SeqCst))
        });
        store.save(&record(A)).expect("save");
        current.store(9, std::sync::atomic::Ordering::SeqCst);
        store.replace(&record(A)).expect("replace");
        assert_eq!(
            store.load(A).expect("load").unwrap().config_generation,
            Some(1),
            "the original planning generation survives a newer session's config"
        );
    }

    #[test]
    fn an_explicit_generation_is_never_overwritten() {
        let (_dir, backing) = store();
        let store = ConfigGenerationStampingStore::new(backing, || Some(2));
        let mut record = record(A);
        record.config_generation = Some(5);
        store.save(&record).expect("save");
        assert_eq!(
            store.load(A).expect("load").unwrap().config_generation,
            Some(5)
        );
    }
}
