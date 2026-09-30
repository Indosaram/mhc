//! `dag/results.test.ts`: persistDagNodeResult durable node artifacts.

use pretty_assertions::assert_eq;
use sha2::{Digest, Sha256};

use super::*;
use crate::dag::store::{DagStoreConfig, DagStoreOptions, create_dag_file_store};
use crate::state::{TaskRecordInput, TaskStatus, create_task_record};
use crate::store::{StateDirConfig, TaskRecordStore};

const RUN_ID: &str = "run-results";
const NODE_ID: &str = "plan";

fn run_stats() -> TaskRunStats {
    TaskRunStats {
        runtime_ms: 4321,
        turns: 3,
        tool_calls: 7,
        output_tokens: Some(512),
        total_tokens: None,
        generation_ms: None,
        tokens_per_second: None,
        cost_usd: Some(0.0125),
        cache_hit_rate_last: None,
        cache_hit_rate_run: None,
    }
}

fn temp_project() -> std::path::PathBuf {
    tempfile::tempdir().expect("tempdir").keep()
}

fn sha256_hex(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn terminal_record(project_dir: &std::path::Path, final_response: &str) -> TaskRecord {
    let records = TaskRecordStore::new(&StateDirConfig {
        project_dir: project_dir.to_path_buf(),
        task_state_dir: None,
    });
    let mut record = create_task_record(
        TaskRecordInput {
            name: None,
            task_summary: None,
            description: None,
            parent_session_id: "parent-session".to_string(),
            root_session_id: "root-session".to_string(),
            depth: 0,
            agent_type: None,
            category: None,
            execution_mode: "direct".to_string(),
            model: "gpt-5.2".to_string(),
            requested_model: None,
            fallback_models: None,
            fallback_attempts: None,
            resolved_model: None,
            tool_allow: None,
            tool_deny: None,
            notify_on_terminal: false,
            pending_steering: None,
            owner: None,
        },
        None,
    )
    .expect("create_task_record");
    record.status = TaskStatus::Completed;
    record.final_response = Some(final_response.to_string());
    record.run_stats = Some(run_stats());
    records.save(&record).expect("save record");
    record
}

fn dag_store(project_dir: &std::path::Path) -> DagFileStore {
    create_dag_file_store(&DagStoreConfig::new(project_dir), DagStoreOptions::default()).expect("dag store opens")
}

#[test]
fn given_a_terminal_node_record_when_persisted_then_the_response_file_carries_the_sha256_and_byte_count_of_the_copy() {
    let project_dir = temp_project();
    let store = dag_store(&project_dir);
    let final_response = "plan complete: shipped 3 files\nwith a trailing note";
    let record = terminal_record(&project_dir, final_response);

    let outcome = persist_dag_node_result(DagNodeResultPersistInput {
        store: &store,
        run_id: RUN_ID.to_string(),
        node_id: NODE_ID.to_string(),
        record: &record,
        now: None,
    });

    let DagNodeResultPersistOutcome::Persisted { artifact } = outcome else {
        panic!("expected persisted outcome");
    };
    assert_eq!(artifact.relative_path, format!("dag/results/{RUN_ID}/{NODE_ID}.txt"));
    assert_eq!(artifact.sha256, sha256_hex(final_response));
    assert_eq!(artifact.bytes, final_response.len() as u64);
    assert_eq!(
        std::fs::read_to_string(store.state_dir.join(&artifact.relative_path)).expect("read artifact"),
        final_response
    );
}

#[test]
fn given_run_stats_on_the_terminal_record_when_persisted_then_a_stats_sidecar_holds_them_with_its_own_digest() {
    let project_dir = temp_project();
    let store = dag_store(&project_dir);
    let record = terminal_record(&project_dir, "build ok");

    let outcome = persist_dag_node_result(DagNodeResultPersistInput {
        store: &store,
        run_id: RUN_ID.to_string(),
        node_id: NODE_ID.to_string(),
        record: &record,
        now: None,
    });

    let DagNodeResultPersistOutcome::Persisted { artifact } = outcome else {
        panic!("expected persisted outcome");
    };
    let sidecar = artifact.stats.expect("expected stats sidecar");
    assert_eq!(sidecar.relative_path, format!("dag/results/{RUN_ID}/{NODE_ID}.stats.json"));
    let raw = std::fs::read_to_string(store.state_dir.join(&sidecar.relative_path)).expect("read sidecar");
    assert_eq!(sidecar.sha256, sha256_hex(&raw));
    assert_eq!(sidecar.bytes, raw.len() as u64);
    let parsed: serde_json::Value = serde_json::from_str(&raw).expect("parse sidecar json");
    assert_eq!(parsed["schemaVersion"], serde_json::json!(1));
    assert_eq!(parsed["runId"], serde_json::json!(RUN_ID));
    assert_eq!(parsed["nodeId"], serde_json::json!(NODE_ID));
    assert_eq!(parsed["runStats"]["runtime_ms"], serde_json::json!(4321));
}

#[test]
fn given_a_persisted_node_when_the_task_record_is_deleted_then_resume_reuse_still_reads_the_output_and_stats() {
    let project_dir = temp_project();
    let store = dag_store(&project_dir);
    let records = TaskRecordStore::new(&StateDirConfig {
        project_dir: project_dir.clone(),
        task_state_dir: None,
    });
    let final_response = "survives the task ttl sweep";
    let record = terminal_record(&project_dir, final_response);
    persist_dag_node_result(DagNodeResultPersistInput {
        store: &store,
        run_id: RUN_ID.to_string(),
        node_id: NODE_ID.to_string(),
        record: &record,
        now: None,
    });

    records.remove(&record.task_id).expect("remove record");

    assert_eq!(records.load(&record.task_id).expect("load"), None);
    let reused = read_dag_node_result(DagNodeResultReadInput {
        store: &store,
        run_id: RUN_ID,
        node_id: NODE_ID,
    })
    .expect("expected reused result");
    assert_eq!(reused.output, final_response);
    assert_eq!(reused.run_stats, Some(run_stats()));
}

#[test]
fn given_an_unwritable_result_path_when_persisting_then_a_journal_corrupt_diagnostic_is_returned_instead_of_throwing()
 {
    let project_dir = temp_project();
    let store = dag_store(&project_dir);
    let record = terminal_record(&project_dir, "unreachable output");
    let output_path = store.paths.result(RUN_ID, NODE_ID);
    std::fs::create_dir_all(&output_path).expect("plant directory at artifact path");
    assert!(output_path.is_dir());

    let outcome = persist_dag_node_result(DagNodeResultPersistInput {
        store: &store,
        run_id: RUN_ID.to_string(),
        node_id: NODE_ID.to_string(),
        record: &record,
        now: None,
    });

    let DagNodeResultPersistOutcome::Failed { diagnostic } = outcome else {
        panic!("expected failed outcome");
    };
    let crate::dag::store::DagStoreDiagnostic::JournalCorrupt { run_id, path, .. } = diagnostic else {
        panic!("expected journal_corrupt diagnostic");
    };
    assert_eq!(run_id, Some(RUN_ID.to_string()));
    assert_eq!(path, output_path.to_string_lossy());

    std::fs::remove_dir_all(&output_path).expect("clear blocked path");
    assert_eq!(
        read_dag_node_result(DagNodeResultReadInput {
            store: &store,
            run_id: RUN_ID,
            node_id: NODE_ID,
        }),
        None
    );
    let retried = persist_dag_node_result(DagNodeResultPersistInput {
        store: &store,
        run_id: RUN_ID.to_string(),
        node_id: NODE_ID.to_string(),
        record: &record,
        now: None,
    });
    assert!(matches!(retried, DagNodeResultPersistOutcome::Persisted { .. }));
}

#[test]
fn given_a_node_with_no_persisted_artifact_when_reuse_reads_it_then_it_reports_nothing_rather_than_falling_back_to_the_record()
 {
    let project_dir = temp_project();
    let store = dag_store(&project_dir);
    let record = terminal_record(&project_dir, "record-only response");
    persist_dag_node_result(DagNodeResultPersistInput {
        store: &store,
        run_id: RUN_ID.to_string(),
        node_id: "build".to_string(),
        record: &record,
        now: None,
    });

    let reused = read_dag_node_result(DagNodeResultReadInput {
        store: &store,
        run_id: RUN_ID,
        node_id: NODE_ID,
    });

    assert_eq!(reused, None);
    assert_eq!(
        read_dag_node_result(DagNodeResultReadInput {
            store: &store,
            run_id: RUN_ID,
            node_id: "build",
        })
        .expect("expected build result")
        .output,
        "record-only response"
    );
}

#[test]
fn given_a_terminal_record_without_run_stats_when_persisted_then_no_sidecar_is_written_and_reuse_reports_undefined_stats()
 {
    let project_dir = temp_project();
    let store = dag_store(&project_dir);
    let mut record = terminal_record(&project_dir, "no stats output");
    record.run_stats = None;

    let outcome = persist_dag_node_result(DagNodeResultPersistInput {
        store: &store,
        run_id: RUN_ID.to_string(),
        node_id: NODE_ID.to_string(),
        record: &record,
        now: None,
    });

    let DagNodeResultPersistOutcome::Persisted { artifact } = outcome else {
        panic!("expected persisted outcome");
    };
    assert_eq!(artifact.stats, None);
    assert!(!store.state_dir.join(format!("dag/results/{RUN_ID}/{NODE_ID}.stats.json")).exists());
    let reused = read_dag_node_result(DagNodeResultReadInput {
        store: &store,
        run_id: RUN_ID,
        node_id: NODE_ID,
    })
    .expect("expected reused result");
    assert_eq!(reused.output, "no stats output");
    assert_eq!(reused.run_stats, None);
}
