//! Port of omo `packages/omo-senpi/src/components/task/store-admission.ts`.

use std::path::Path;

use senpi_task::runners::rpc_host::store_index::{register_store_index, task_store_index_path};

pub async fn admit_task_store(agent_dir: &Path, store_dir: &Path, context: &str) -> bool {
    let index_path = task_store_index_path(agent_dir);
    match register_store_index(index_path, store_dir.to_path_buf(), now_millis()).await {
        Ok(()) => true,
        Err(error) => {
            eprintln!(
                "omo-senpi task {context} skipped: the task store index is unavailable: {error}"
            );
            false
        }
    }
}

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn an_admitted_store_lands_in_the_agent_dir_index() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = dir.path().join("store");
        std::fs::create_dir_all(&store).expect("store dir");
        assert!(admit_task_store(dir.path(), &store, "prewarm").await);
        let index = senpi_task::runners::rpc_host::store_index::read_task_store_index(
            &task_store_index_path(dir.path()),
        )
        .expect("index reads");
        assert_eq!(index.stores.len(), 1);
    }

    #[tokio::test]
    async fn a_corrupt_index_reports_the_admission_as_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let index = task_store_index_path(dir.path());
        std::fs::create_dir_all(index.parent().expect("parent")).expect("rpc dir");
        std::fs::write(&index, b"{\"version\":2,\"stores\":{}}").expect("corrupt index");
        let store = dir.path().join("store");
        std::fs::create_dir_all(&store).expect("store dir");
        assert!(
            !admit_task_store(dir.path(), &store, "prewarm").await,
            "a non-version-1 index must be reported as refused"
        );
        assert_eq!(
            std::fs::read(&index).expect("index survives"),
            b"{\"version\":2,\"stores\":{}}",
            "a refused admission leaves the index byte-identical"
        );
    }
}
