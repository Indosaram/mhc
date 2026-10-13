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
        let mut payload=json!({"run_id":run.run_id,"run_key":run.run_key,"name":run.name,"status":run.status,"created_at":run.created_at,"updated_at":updated,"counts":run.counts,"nodes":nodes,"edges":run.edges,"waves":waves});
        if let Some(completed_at)=&run.completed_at { payload["completed_at"]=json!(completed_at); }
        if let Some(pid)=run.lease_holder_pid { payload["lease_holder_pid"]=json!(pid); }
        if let Some(history)=&run.amend_history && !history.is_empty() { payload["amend_count"]=json!(history.len()); }
        if let Some(nodes)=payload["nodes"].as_array_mut() { for (index,node) in run.nodes.iter().enumerate() { if let Some(error)=&node.error { nodes[index]["last_error"]=json!({"code":error.code,"message":error.message}); } } }
        payload
    }).collect();
    let mut value = json!({"parent_session_id":parent_session_id,"runs":payloads});
    if runs.len() > DAG_MAX_RUN_SNAPSHOTS { value["truncated_runs"] = json!(runs.len() - DAG_MAX_RUN_SNAPSHOTS); }
    value
}
