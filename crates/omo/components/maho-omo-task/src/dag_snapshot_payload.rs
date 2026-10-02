use serde_json::{Value, json};
use senpi_task::dag::types::DagRunSnapshot;

pub const DAG_MAX_RUN_SNAPSHOTS: usize = 256;
pub fn dag_updated_payload(parent_session_id: &str, runs: &[(DagRunSnapshot, String)]) -> Value {
    let payloads: Vec<_> = runs.iter().take(DAG_MAX_RUN_SNAPSHOTS).map(|(run, updated)| {
        let nodes: Vec<_> = run.nodes.iter().map(|node| {
            let mut value = json!({"id":node.id,"prompt":node.prompt,"depends_on":node.depends_on,"state":node.state,"attempt":node.attempt,"created_at":node.created_at});
            for (key, field) in [("label", &node.label), ("task_id", &node.task_id), ("started_at", &node.started_at), ("completed_at", &node.completed_at)] { if let Some(field) = field { value[key] = json!(field); } }
            value
        }).collect();
        let waves: Vec<_> = run.waves.iter().map(|wave| json!({"index":wave.index,"node_ids":wave.node_ids})).collect();
        json!({"run_id":run.run_id,"run_key":run.run_key,"name":run.name,"status":run.status,"created_at":run.created_at,"updated_at":updated,"counts":run.counts,"nodes":nodes,"edges":run.edges,"waves":waves})
    }).collect();
    let mut value = json!({"parent_session_id":parent_session_id,"runs":payloads});
    if runs.len() > DAG_MAX_RUN_SNAPSHOTS { value["truncated_runs"] = json!(runs.len() - DAG_MAX_RUN_SNAPSHOTS); }
    value
}
