use std::sync::Arc;
use maho_omo_task::dag_snapshot_payload::dag_updated_payload;
use senpi_task::dag::{manager::{create_dag_manager,DagManagerOptions,DagStartParams},store::{create_dag_file_store,DagStoreConfig,DagStoreOptions},types::DagRunSnapshot};
use serde_json::json;
fn snapshot()->(tempfile::TempDir,DagRunSnapshot) {
    let root=tempfile::tempdir().expect("root"); let store=create_dag_file_store(&DagStoreConfig::new(root.path()),DagStoreOptions::default()).expect("store"); let manager=create_dag_manager(DagManagerOptions { store:Arc::new(store),new_run_id:Some(Arc::new(|| "run".into())),now:Some(Arc::new(|| 1000)),materialize_skills:None,settings:None });
    let definition=senpi_task::dag::graph::DagDefinition { key:"key".into(),name:"release".into(),nodes:vec![senpi_task::dag::graph::DagNodeInput { id:"a".into(),prompt:"work".into(),target:senpi_task::dag::types::DagNodeTarget::Category("quick".into()),label:None,depends_on:None,task_summary:None,description:None,load_skills:None }] }; let run=manager.start(DagStartParams { definition,parent_session_id:"parent".into(),root_session_id:"root".into() }).expect("run").snapshot; (root,run)
}
#[test] fn snapshot_cap_reports_only_actual_overflow() {
    let (_root,run)=snapshot(); let mut runs=(0..256).map(|index| { let mut run=run.clone(); run.run_id=format!("run-{index}"); (run,"updated".into()) }).collect::<Vec<_>>();
    let value=dag_updated_payload("parent",&runs); assert_eq!(value["runs"].as_array().expect("runs").len(),256); assert!(value.get("truncated_runs").is_none()); runs.push((run,"updated".into())); let value=dag_updated_payload("parent",&runs); assert_eq!(value["truncated_runs"],1); assert_eq!(value["runs"].as_array().expect("runs").len(),256); assert_eq!(value["runs"][255]["run_id"],"run-255");
}
#[test] fn wire_payload_is_snake_case_with_absent_optional_node_fields() {
    let (_root,mut run)=snapshot(); let value=dag_updated_payload("parent",&[(run.clone(),"updated".into())]); assert_eq!(value["parent_session_id"],"parent"); assert_eq!(value["runs"][0]["updated_at"],"updated"); assert_eq!(value["runs"][0]["waves"][0]["node_ids"],json!(["a"]));
    for key in ["label","task_id","started_at","completed_at"] { assert!(value["runs"][0]["nodes"][0].get(key).is_none()); }
    run.nodes[0].label=Some("label".into()); run.nodes[0].task_id=Some("st_a".into()); run.nodes[0].started_at=Some("start".into()); run.nodes[0].completed_at=Some("end".into()); let value=dag_updated_payload("parent",&[(run,"updated".into())]); let node=&value["runs"][0]["nodes"][0]; assert_eq!(node["label"],"label"); assert_eq!(node["task_id"],"st_a"); assert_eq!(node["started_at"],"start"); assert_eq!(node["completed_at"],"end"); assert!(node.get("taskId").is_none());
}
