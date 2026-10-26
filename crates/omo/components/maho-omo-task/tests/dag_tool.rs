use std::sync::{Arc, Mutex};
use std::fs;
use serde_json::json;

use maho_ext_api::{ToolContent, ToolError};
use maho_omo_task::dag_tool::{
    WORKFLOW_TOOL_NAME, create_dag_tool, run_dag_tool,
    DagToolDeps, DagToolInput, DefinitionInput, NodeInput, DagToolSendOutcome, DagToolSendDelivery,
    DagToolExecutionError,
};
use senpi_task::dag::{
    manager::{create_dag_manager, DagManager, DagManagerError, DagManagerErrorCode, DagManagerOptions},
    scheduler::{DagNodeControlError, DagNodeControlErrorCode},
    store::{create_dag_file_store, DagFileStore, DagStoreConfig, DagStoreOptions},
};

const PARENT_SESSION_ID: &str = "ses_parent";
const ROOT_SESSION_ID: &str = "ses_root";
const FOREIGN_SESSION_ID: &str = "ses_foreign";

struct Fixture {
    manager: DagManager,
    store: Arc<DagFileStore>,
    _temp_dir: tempfile::TempDir,
}

#[test]
fn given_workflow_schema_when_parameters_are_registered_then_only_latest_actions_are_exposed() {
    let f = fixture();
    let schema = create_dag_tool(deps(&f.manager)).parameters;
    let properties = schema["properties"].as_object().expect("schema properties");
    let actions = properties["action"]["anyOf"].as_array().expect("action choices");
    assert_eq!(actions.len(), 11);
    assert!(actions.iter().all(|action| action["const"] != "detach"));
    assert!(properties.get("detach").is_none());
}

#[test]
fn given_send_message_with_surrounding_whitespace_when_dispatching_send_then_original_message_is_preserved() {
    let f = fixture();
    let started = run_dag_tool(&deps(&f.manager), DagToolInput {
        action: "start".into(), definition: Some(default_definition()), run_id: None,
        reason: None, detach: None, node_id: None, node_ids: None, message: None, prompt: None,
    guidance_id: None,
    text: None,
    remove: None,
    }).expect("start");
    let run_id = started.details.expect("details")["run_id"].as_str().expect("run id").to_owned();
    let sent = Arc::new(Mutex::new(String::new()));
    let seen = sent.clone();
    let mut dependencies = deps(&f.manager);
    dependencies.send = Some(Arc::new(move |_, _, message| {
        *seen.lock().expect("sent mutex") = message.to_owned();
        Ok(DagToolSendOutcome { node_id: "plan".into(), task_id: Some("task-1".into()), delivery: DagToolSendDelivery::Steer, guidance_id: None, effect_id: None, queue_position: None })
    }));
    run_dag_tool(&dependencies, DagToolInput {
        action: "send".into(), definition: None, run_id: Some(run_id), reason: None,
        detach: None, node_id: Some("plan".into()), node_ids: None, message: Some("  continue  ".into()), prompt: None,
    guidance_id: None,
    text: None,
    remove: None,
    }).expect("send");
    assert_eq!(*sent.lock().expect("sent mutex"), "  continue  ");
}

impl Fixture {
    fn run_file_count(&self) -> usize {
        let runs_path = &self.store.paths.runs;
        if !runs_path.exists() {
            return 0;
        }
        fs::read_dir(runs_path)
            .expect("read_dir")
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "json"))
            .count()
    }
}

fn fixture() -> Fixture {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let project_dir = temp_dir.path().to_path_buf();
    let store = Arc::new(create_dag_file_store(
        &DagStoreConfig::new(&project_dir),
        DagStoreOptions::default(),
    ).expect("store"));
    let counter = Arc::new(Mutex::new(0));
    let new_run_id = {
        let counter = counter.clone();
        Arc::new(move || {
            let mut c = counter.lock().expect("lock");
            *c += 1;
            format!("run-{}", *c)
        })
    };
    let manager = create_dag_manager(DagManagerOptions {
        store: store.clone(),
        new_run_id: Some(new_run_id),
        now: None,
        materialize_skills: None,
        settings: None,
    });
    Fixture {
        manager,
        store,
        _temp_dir: temp_dir,
    }
}

fn default_definition() -> DefinitionInput {
    DefinitionInput {
        key: "release-plan".into(),
        name: "release plan".into(),
        nodes: vec![
            NodeInput {
                id: "plan".into(),
                prompt: "draft the plan".into(),
                label: None,
                category: Some("quick".into()),
                subagent_type: None,
                model: None,
                depends_on: None,
                task_summary: None,
                description: None,
                load_skills: None,
            },
            NodeInput {
                id: "build".into(),
                prompt: "build it".into(),
                label: None,
                category: Some("quick".into()),
                subagent_type: None,
                model: None,
                depends_on: Some(vec!["plan".into()]),
                task_summary: None,
                description: None,
                load_skills: None,
            },
        ],
    }
}

fn deps(manager: &DagManager) -> DagToolDeps {
    DagToolDeps {
        manager: manager.clone(),
        parent_session_id: Arc::new(|| PARENT_SESSION_ID.into()),
        root_session_id: Arc::new(|| ROOT_SESSION_ID.into()),
        wait: None,
        cancel: None,
        retry: None,
        send: None,
        amend:None,

    cancel_node:None,

    guide:None,

    pause_reason:None,

    }
}

fn get_text(content: &ToolContent) -> &str {
    match content {
        ToolContent::Text { text, .. } => text.as_str(),
        ToolContent::Image { .. } => "",
    }
}

#[test]
fn workflow_tool_registration() {
    let f = fixture();
    let tool = create_dag_tool(deps(&f.manager));
    assert_eq!(tool.name, "workflow");
    assert_eq!(WORKFLOW_TOOL_NAME, "workflow");
}

#[test]
fn start_action_roundtrips_through_manager() {
    let f = fixture();
    let result = run_dag_tool(
        &deps(&f.manager),
        DagToolInput {
            action: "start".into(),
            definition: Some(default_definition()),
            run_id: None,
            reason: None,
            detach: None,
            node_id: None,
            node_ids: None,
            message: None,
            prompt: None,
        guidance_id: None,
        text: None,
        remove: None,
        },
    ).unwrap();
    let details = result.details.unwrap();
    assert_eq!(details["kind"], "started");
    let run_id = details["run_id"].as_str().unwrap();
    assert_eq!(run_id, "run-1");
    let node_ids: Vec<&str> = details["snapshot"]["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["id"].as_str().unwrap())
        .collect();
    assert_eq!(node_ids, vec!["plan", "build"]);
    assert_eq!(f.run_file_count(), 1);
    assert_eq!(f.manager.snapshot(&run_id.into(), PARENT_SESSION_ID).unwrap().run_id, run_id);
}

#[test]
fn repeat_start_reuses_same_run() {
    let f = fixture();
    let d = deps(&f.manager);
    run_dag_tool(
        &d,
        DagToolInput {
            action: "start".into(),
            definition: Some(default_definition()),
            run_id: None,
            reason: None,
            detach: None,
            node_id: None,
            node_ids: None,
            message: None,
            prompt: None,
        guidance_id: None,
        text: None,
        remove: None,
        },
    ).unwrap();

    let second = run_dag_tool(
        &d,
        DagToolInput {
            action: "start".into(),
            definition: Some(default_definition()),
            run_id: None,
            reason: None,
            detach: None,
            node_id: None,
            node_ids: None,
            message: None,
            prompt: None,
        guidance_id: None,
        text: None,
        remove: None,
        },
    ).unwrap();
    let details = second.details.unwrap();
    assert_eq!(details["kind"], "started");
    assert_eq!(details["run_id"], "run-1");
    assert_eq!(details["reused"], true);
    assert_eq!(f.run_file_count(), 1);
}

#[test]
fn start_without_definition_rejects() {
    let f = fixture();
    let result = run_dag_tool(
        &deps(&f.manager),
        DagToolInput {
            action: "start".into(),
            definition: None,
            run_id: None,
            reason: None,
            detach: None,
            node_id: None,
            node_ids: None,
            message: None,
            prompt: None,
        guidance_id: None,
        text: None,
        remove: None,
        },
    ).unwrap();
    let details = result.details.unwrap();
    assert_eq!(details["kind"], "error");
    assert_eq!(details["error"]["code"], "invalid_definition");
    assert_eq!(f.run_file_count(), 0);
}

#[test]
fn cyclic_definition_rejected_with_diagnostics() {
    let f = fixture();
    let cyclic = DefinitionInput {
        key: "release-plan".into(),
        name: "release plan".into(),
        nodes: vec![
            NodeInput {
                id: "a".into(),
                prompt: "a".into(),
                label: None,
                category: Some("quick".into()),
                subagent_type: None,
                model: None,
                depends_on: Some(vec!["b".into()]),
                task_summary: None,
                description: None,
                load_skills: None,
            },
            NodeInput {
                id: "b".into(),
                prompt: "b".into(),
                label: None,
                category: Some("quick".into()),
                subagent_type: None,
                model: None,
                depends_on: Some(vec!["a".into()]),
                task_summary: None,
                description: None,
                load_skills: None,
            },
        ],
    };
    let result = run_dag_tool(
        &deps(&f.manager),
        DagToolInput {
            action: "start".into(),
            definition: Some(cyclic),
            run_id: None,
            reason: None,
            detach: None,
            node_id: None,
            node_ids: None,
            message: None,
            prompt: None,
        guidance_id: None,
        text: None,
        remove: None,
        },
    ).unwrap();
    let details = result.details.unwrap();
    assert_eq!(details["kind"], "error");
    assert_eq!(details["error"]["code"], "invalid_definition");
    let error_codes: Vec<&str> = details["error"]["errors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["code"].as_str().unwrap())
        .collect();
    assert!(error_codes.contains(&"cycle"));
    assert!(!details["error"]["diagnostics"].as_array().unwrap().is_empty());
    assert_eq!(f.run_file_count(), 0);
}

#[test]
fn category_with_model_rejected() {
    let f = fixture();
    let conflicted = DefinitionInput {
        key: "release-plan".into(),
        name: "release plan".into(),
        nodes: vec![NodeInput {
            id: "plan".into(),
            prompt: "draft".into(),
            label: None,
            category: Some("quick".into()),
            subagent_type: None,
            model: Some("anthropic/claude-opus-4".into()),
            depends_on: None,
            task_summary: None,
            description: None,
            load_skills: None,
        }],
    };
    let result = run_dag_tool(
        &deps(&f.manager),
        DagToolInput {
            action: "start".into(),
            definition: Some(conflicted),
            run_id: None,
            reason: None,
            detach: None,
            node_id: None,
            node_ids: None,
            message: None,
            prompt: None,
        guidance_id: None,
        text: None,
        remove: None,
        },
    ).unwrap();
    let details = result.details.unwrap();
    assert_eq!(details["kind"], "error");
    assert_eq!(details["error"]["code"], "invalid_definition");
    assert_eq!(details["error"]["nodes"][0]["code"], "category_with_model");
    assert_eq!(details["error"]["nodes"][0]["node_id"], "plan");
    assert_eq!(f.run_file_count(), 0);
}

#[test]
fn both_targets_rejected() {
    let f = fixture();
    let conflicted = DefinitionInput {
        key: "release-plan".into(),
        name: "release plan".into(),
        nodes: vec![NodeInput {
            id: "plan".into(),
            prompt: "draft".into(),
            label: None,
            category: Some("quick".into()),
            subagent_type: Some("plan-reviewer".into()),
            model: None,
            depends_on: None,
            task_summary: None,
            description: None,
            load_skills: None,
        }],
    };
    let result = run_dag_tool(
        &deps(&f.manager),
        DagToolInput {
            action: "start".into(),
            definition: Some(conflicted),
            run_id: None,
            reason: None,
            detach: None,
            node_id: None,
            node_ids: None,
            message: None,
            prompt: None,
        guidance_id: None,
        text: None,
        remove: None,
        },
    ).unwrap();
    let details = result.details.unwrap();
    assert_eq!(details["kind"], "error");
    assert_eq!(details["error"]["nodes"][0]["code"], "both_targets");
    assert_eq!(f.run_file_count(), 0);
}

#[test]
fn no_target_rejected() {
    let f = fixture();
    let targetless = DefinitionInput {
        key: "release-plan".into(),
        name: "release plan".into(),
        nodes: vec![NodeInput {
            id: "plan".into(),
            prompt: "draft".into(),
            label: None,
            category: None,
            subagent_type: None,
            model: None,
            depends_on: None,
            task_summary: None,
            description: None,
            load_skills: None,
        }],
    };
    let result = run_dag_tool(
        &deps(&f.manager),
        DagToolInput {
            action: "start".into(),
            definition: Some(targetless),
            run_id: None,
            reason: None,
            detach: None,
            node_id: None,
            node_ids: None,
            message: None,
            prompt: None,
        guidance_id: None,
        text: None,
        remove: None,
        },
    ).unwrap();
    let details = result.details.unwrap();
    assert_eq!(details["kind"], "error");
    assert_eq!(details["error"]["nodes"][0]["code"], "no_target");
    assert_eq!(f.run_file_count(), 0);
}

#[test]
fn subagent_type_and_model_accepted() {
    let f = fixture();
    let explicit = DefinitionInput {
        key: "release-plan".into(),
        name: "release plan".into(),
        nodes: vec![NodeInput {
            id: "plan".into(),
            prompt: "draft".into(),
            label: None,
            category: None,
            subagent_type: Some("plan-reviewer".into()),
            model: Some("anthropic/claude-opus-4".into()),
            depends_on: None,
            task_summary: None,
            description: None,
            load_skills: None,
        }],
    };
    let result = run_dag_tool(
        &deps(&f.manager),
        DagToolInput {
            action: "start".into(),
            definition: Some(explicit),
            run_id: None,
            reason: None,
            detach: None,
            node_id: None,
            node_ids: None,
            message: None,
            prompt: None,
        guidance_id: None,
        text: None,
        remove: None,
        },
    ).unwrap();
    let details = result.details.unwrap();
    assert_eq!(details["kind"], "started");
    let node = &details["snapshot"]["nodes"][0];
    assert_eq!(node["route"]["kind"], "agent");
    assert_eq!(node["route"]["agent"], "plan-reviewer");
    assert_eq!(node["route"]["model"], "anthropic/claude-opus-4");
}

#[test]
fn attach_roundtrips_live_snapshot() {
    let f = fixture();
    let d = deps(&f.manager);
    let started = run_dag_tool(
        &d,
        DagToolInput {
            action: "start".into(),
            definition: Some(default_definition()),
            run_id: None,
            reason: None,
            detach: None,
            node_id: None,
            node_ids: None,
            message: None,
            prompt: None,
        guidance_id: None,
        text: None,
        remove: None,
        },
    ).unwrap();
    let run_id = started.details.unwrap()["run_id"].as_str().unwrap().to_string();

    let result = run_dag_tool(
        &d,
        DagToolInput {
            action: "attach".into(),
            definition: None,
            run_id: Some(run_id.clone()),
            reason: None,
            detach: None,
            node_id: None,
            node_ids: None,
            message: None,
            prompt: None,
        guidance_id: None,
        text: None,
        remove: None,
        },
    ).unwrap();
    let details = result.details.unwrap();
    assert_eq!(details["kind"], "attached");
    assert_eq!(details["run_id"], run_id);
    assert_eq!(details["snapshot"]["counts"]["total"], 2);
}

#[test]
fn snapshot_returns_manager_projection() {
    let f = fixture();
    let d = deps(&f.manager);
    let started = run_dag_tool(
        &d,
        DagToolInput {
            action: "start".into(),
            definition: Some(default_definition()),
            run_id: None,
            reason: None,
            detach: None,
            node_id: None,
            node_ids: None,
            message: None,
            prompt: None,
        guidance_id: None,
        text: None,
        remove: None,
        },
    ).unwrap();
    let run_id = started.details.unwrap()["run_id"].as_str().unwrap().to_string();

    let result = run_dag_tool(
        &d,
        DagToolInput {
            action: "snapshot".into(),
            definition: None,
            run_id: Some(run_id.clone()),
            reason: None,
            detach: None,
            node_id: None,
            node_ids: None,
            message: None,
            prompt: None,
        guidance_id: None,
        text: None,
        remove: None,
        },
    ).unwrap();
    let details = result.details.unwrap();
    assert_eq!(details["kind"], "snapshot");
    let expected = serde_json::to_value(f.manager.snapshot(&run_id, PARENT_SESSION_ID).unwrap()).unwrap();
    assert_eq!(details["snapshot"], expected);
}

#[test]
fn snapshot_unknown_run_rejects() {
    let f = fixture();
    let result = run_dag_tool(
        &deps(&f.manager),
        DagToolInput {
            action: "snapshot".into(),
            definition: None,
            run_id: Some("run-missing".into()),
            reason: None,
            detach: None,
            node_id: None,
            node_ids: None,
            message: None,
            prompt: None,
        guidance_id: None,
        text: None,
        remove: None,
        },
    ).unwrap();
    let details = result.details.unwrap();
    assert_eq!(details["kind"], "error");
    assert_eq!(details["error"]["code"], "run_not_found");
}

#[test]
fn snapshot_foreign_session_rejects() {
    let f = fixture();
    let started = run_dag_tool(
        &deps(&f.manager),
        DagToolInput {
            action: "start".into(),
            definition: Some(default_definition()),
            run_id: None,
            reason: None,
            detach: None,
            node_id: None,
            node_ids: None,
            message: None,
            prompt: None,
        guidance_id: None,
        text: None,
        remove: None,
        },
    ).unwrap();
    let run_id = started.details.unwrap()["run_id"].as_str().unwrap().to_string();

    let foreign = DagToolDeps {
        manager: f.manager.clone(),
        parent_session_id: Arc::new(|| FOREIGN_SESSION_ID.into()),
        root_session_id: Arc::new(|| ROOT_SESSION_ID.into()),
        wait: None,
        cancel: None,
        retry: None,
        send: None,
        amend:None,

    cancel_node:None,

    guide:None,

    pause_reason:None,

    };
    let result = run_dag_tool(
        &foreign,
        DagToolInput {
            action: "snapshot".into(),
            definition: None,
            run_id: Some(run_id),
            reason: None,
            detach: None,
            node_id: None,
            node_ids: None,
            message: None,
            prompt: None,
        guidance_id: None,
        text: None,
        remove: None,
        },
    ).unwrap();
    let details = result.details.unwrap();
    assert_eq!(details["kind"], "error");
    assert_eq!(details["error"]["code"], "run_not_owned");
}

#[test]
fn wait_with_detach_disabled_blocks_and_resolves() {
    let f = fixture();
    let started = run_dag_tool(
        &deps(&f.manager),
        DagToolInput {
            action: "start".into(),
            definition: Some(default_definition()),
            run_id: None,
            reason: None,
            detach: None,
            node_id: None,
            node_ids: None,
            message: None,
            prompt: None,
        guidance_id: None,
        text: None,
        remove: None,
        },
    ).unwrap();
    let run_id = started.details.unwrap()["run_id"].as_str().unwrap().to_string();

    let waited = Arc::new(Mutex::new(Vec::new()));
    let waited_clone = waited.clone();
    let manager_clone = f.manager.clone();
    let with_wait = DagToolDeps {
        manager: f.manager.clone(),
        parent_session_id: Arc::new(|| PARENT_SESSION_ID.into()),
        root_session_id: Arc::new(|| ROOT_SESSION_ID.into()),
        wait: Some(Arc::new(move |run: &str, session: &str| {
            waited_clone.lock().unwrap().push(format!("{run}:{session}"));
            let snap = manager_clone.snapshot(&run.to_string(), session).unwrap();
            Ok(json!({
                "runId": run,
                "status": "completed",
                "snapshot": snap,
                "nodes": {},
            }))
        })),
        cancel: None,
        retry: None,
        send: None,
        amend:None,

    cancel_node:None,

    guide:None,

    pause_reason:None,

    };

    let result = run_dag_tool(
        &with_wait,
        DagToolInput {
            action: "wait".into(),
            definition: None,
            run_id: Some(run_id.clone()),
            reason: None,
            detach: Some(false),
            node_id: None,
            node_ids: None,
            message: None,
            prompt: None,
        guidance_id: None,
        text: None,
        remove: None,
        },
    ).unwrap();
    let details = result.details.unwrap();
    assert_eq!(details["kind"], "waited");
    assert_eq!(details["result"]["status"], "completed");
    assert_eq!(*waited.lock().unwrap(), vec![format!("{run_id}:{PARENT_SESSION_ID}")]);
}

#[test]
fn wait_with_default_detach_returns_detached() {
    let f = fixture();
    let started = run_dag_tool(
        &deps(&f.manager),
        DagToolInput {
            action: "start".into(),
            definition: Some(default_definition()),
            run_id: None,
            reason: None,
            detach: None,
            node_id: None,
            node_ids: None,
            message: None,
            prompt: None,
        guidance_id: None,
        text: None,
        remove: None,
        },
    ).unwrap();
    let run_id = started.details.unwrap()["run_id"].as_str().unwrap().to_string();

    let waited = Arc::new(Mutex::new(Vec::new()));
    let waited_clone = waited.clone();
    let with_wait = DagToolDeps {
        manager: f.manager.clone(),
        parent_session_id: Arc::new(|| PARENT_SESSION_ID.into()),
        root_session_id: Arc::new(|| ROOT_SESSION_ID.into()),
        wait: Some(Arc::new(move |run: &str, session: &str| {
            waited_clone.lock().unwrap().push(format!("{run}:{session}"));
            Ok(json!({"status": "completed"}))
        })),
        cancel: None,
        retry: None,
        send: None,
        amend:None,

    cancel_node:None,

    guide:None,

    pause_reason:None,

    };

    let result = run_dag_tool(
        &with_wait,
        DagToolInput {
            action: "wait".into(),
            definition: None,
            run_id: Some(run_id.clone()),
            reason: None,
            detach: None,
            node_id: None,
            node_ids: None,
            message: None,
            prompt: None,
        guidance_id: None,
        text: None,
        remove: None,
        },
    ).unwrap();
    let details = result.details.unwrap();
    assert_eq!(details["kind"], "detached");
    assert_eq!(details["run_id"], run_id);
    assert_eq!(details["snapshot"]["status"], "pending");
    assert!(waited.lock().unwrap().is_empty());
    assert!(get_text(&result.content[0]).contains("woken as each node completes"));
}

#[test]
fn wait_settled_run_returns_final_result() {
    let f = fixture();
    let started = run_dag_tool(
        &deps(&f.manager),
        DagToolInput {
            action: "start".into(),
            definition: Some(default_definition()),
            run_id: None,
            reason: None,
            detach: None,
            node_id: None,
            node_ids: None,
            message: None,
            prompt: None,
        guidance_id: None,
        text: None,
        remove: None,
        },
    ).unwrap();
    let run_id = started.details.unwrap()["run_id"].as_str().unwrap().to_string();

    let mut record: serde_json::Value = f.store.read_checkpoint(&run_id).unwrap().expect("checkpoint");
    record["status"] = json!("completed");
    f.store.write_checkpoint(&run_id, &record).unwrap();

    let with_wait = DagToolDeps {
        manager: f.manager.clone(),
        parent_session_id: Arc::new(|| PARENT_SESSION_ID.into()),
        root_session_id: Arc::new(|| ROOT_SESSION_ID.into()),
        wait: Some(Arc::new(move |run: &str, _session: &str| {
            Ok(json!({
                "runId": run,
                "status": "completed",
                "nodes": {},
            }))
        })),
        cancel: None,
        retry: None,
        send: None,
        amend:None,

    cancel_node:None,

    guide:None,

    pause_reason:None,

    };

    let result = run_dag_tool(
        &with_wait,
        DagToolInput {
            action: "wait".into(),
            definition: None,
            run_id: Some(run_id),
            reason: None,
            detach: None,
            node_id: None,
            node_ids: None,
            message: None,
            prompt: None,
        guidance_id: None,
        text: None,
        remove: None,
        },
    ).unwrap();
    let details = result.details.unwrap();
    assert_eq!(details["kind"], "waited");
    assert_eq!(details["result"]["status"], "completed");
}

#[test]
fn wait_without_wait_surface_enforces_ownership() {
    let f = fixture();
    let result = run_dag_tool(
        &deps(&f.manager),
        DagToolInput {
            action: "wait".into(),
            definition: None,
            run_id: Some("run-missing".into()),
            reason: None,
            detach: None,
            node_id: None,
            node_ids: None,
            message: None,
            prompt: None,
        guidance_id: None,
        text: None,
        remove: None,
        },
    ).unwrap();
    let details = result.details.unwrap();
    assert_eq!(details["kind"], "error");
    assert_eq!(details["error"]["code"], "run_not_found");
}

#[test]
fn cancel_action_forwards_reason() {
    let f = fixture();
    let started = run_dag_tool(
        &deps(&f.manager),
        DagToolInput {
            action: "start".into(),
            definition: Some(default_definition()),
            run_id: None,
            reason: None,
            detach: None,
            node_id: None,
            node_ids: None,
            message: None,
            prompt: None,
        guidance_id: None,
        text: None,
        remove: None,
        },
    ).unwrap();
    let run_id = started.details.unwrap()["run_id"].as_str().unwrap().to_string();

    let cancelled = Arc::new(Mutex::new(Vec::new()));
    let cancelled_clone = cancelled.clone();
    let with_cancel = DagToolDeps {
        manager: f.manager.clone(),
        parent_session_id: Arc::new(|| PARENT_SESSION_ID.into()),
        root_session_id: Arc::new(|| ROOT_SESSION_ID.into()),
        wait: None,
        cancel: Some(Arc::new(move |run: &str, reason: Option<&str>| {
            cancelled_clone.lock().unwrap().push(format!("{run}:{}", reason.unwrap_or_default()));
            Ok(())
        })),
        retry: None,
        send: None,
        amend:None,

    cancel_node:None,

    guide:None,

    pause_reason:None,

    };

    let result = run_dag_tool(
        &with_cancel,
        DagToolInput {
            action: "cancel".into(),
            definition: None,
            run_id: Some(run_id.clone()),
            reason: Some("superseded".into()),
            detach: None,
            node_id: None,
            node_ids: None,
            message: None,
            prompt: None,
        guidance_id: None,
        text: None,
        remove: None,
        },
    ).unwrap();
    let details = result.details.unwrap();
    assert_eq!(details["kind"], "cancelled");
    assert_eq!(*cancelled.lock().unwrap(), vec![format!("{run_id}:superseded")]);
}

#[test]
fn cancel_foreign_run_rejects() {
    let f = fixture();
    let started = run_dag_tool(
        &deps(&f.manager),
        DagToolInput {
            action: "start".into(),
            definition: Some(default_definition()),
            run_id: None,
            reason: None,
            detach: None,
            node_id: None,
            node_ids: None,
            message: None,
            prompt: None,
        guidance_id: None,
        text: None,
        remove: None,
        },
    ).unwrap();
    let run_id = started.details.unwrap()["run_id"].as_str().unwrap().to_string();

    let cancelled = Arc::new(Mutex::new(Vec::new()));
    let cancelled_clone = cancelled.clone();
    let foreign = DagToolDeps {
        manager: f.manager.clone(),
        parent_session_id: Arc::new(|| FOREIGN_SESSION_ID.into()),
        root_session_id: Arc::new(|| ROOT_SESSION_ID.into()),
        wait: None,
        cancel: Some(Arc::new(move |run: &str, _: Option<&str>| {
            cancelled_clone.lock().unwrap().push(run.to_string());
            Ok(())
        })),
        retry: None,
        send: None,
        amend:None,

    cancel_node:None,

    guide:None,

    pause_reason:None,

    };

    let result = run_dag_tool(
        &foreign,
        DagToolInput {
            action: "cancel".into(),
            definition: None,
            run_id: Some(run_id),
            reason: None,
            detach: None,
            node_id: None,
            node_ids: None,
            message: None,
            prompt: None,
        guidance_id: None,
        text: None,
        remove: None,
        },
    ).unwrap();
    let details = result.details.unwrap();
    assert_eq!(details["kind"], "error");
    assert_eq!(details["error"]["code"], "run_not_owned");
    assert!(cancelled.lock().unwrap().is_empty());
}

#[test]
fn start_warnings_on_contract_violation() {
    let f = fixture();
    let violating = DefinitionInput {
        key: "release-plan".into(),
        name: "release plan".into(),
        nodes: vec![
            NodeInput {
                id: "plan".into(),
                prompt: "draft the plan".into(),
                label: None,
                category: Some("quick".into()),
                subagent_type: None,
                model: None,
                depends_on: None,
                task_summary: None,
                description: None,
                load_skills: None,
            },
            NodeInput {
                id: "build".into(),
                prompt: "build it".into(),
                label: None,
                category: Some("quick".into()),
                subagent_type: None,
                model: None,
                depends_on: Some(vec!["plan".into()]),
                task_summary: None,
                description: None,
                load_skills: None,
            },
        ],
    };
    let result = run_dag_tool(
        &deps(&f.manager),
        DagToolInput {
            action: "start".into(),
            definition: Some(violating),
            run_id: None,
            reason: None,
            detach: None,
            node_id: None,
            node_ids: None,
            message: None,
            prompt: None,
        guidance_id: None,
        text: None,
        remove: None,
        },
    ).unwrap();
    let details = result.details.unwrap();
    assert_eq!(details["kind"], "started");
    let warnings = details["warnings"].as_array().unwrap();
    assert!(warnings.iter().any(|w| w.as_str().unwrap().contains("\"plan\"") && w.as_str().unwrap().contains("TASK:")));
    for marker in ["DELIVERABLE:", "SCOPE:", "VERIFY:"] {
        assert!(warnings.iter().any(|warning| warning.as_str().unwrap().contains(marker)), "missing {marker} warning");
    }
    assert!(warnings.iter().any(|w| w.as_str().unwrap().contains("verification")));
    assert!(get_text(&result.content[0]).contains("warning"));
}

#[test]
fn contract_complete_definition_has_empty_warnings() {
    let f = fixture();
    let compliant = DefinitionInput {
        key: "release-plan".into(),
        name: "release plan".into(),
        nodes: vec![
            NodeInput {
                id: "plan".into(),
                prompt: "TASK: draft the plan. DELIVERABLE: plan.md. SCOPE: write plan.md only. VERIFY: test -f plan.md. STOP WHEN: plan.md exists.".into(),
                label: None,
                category: Some("quick".into()),
                subagent_type: None,
                model: None,
                depends_on: None,
                task_summary: None,
                description: None,
                load_skills: None,
            },
            NodeInput {
                id: "verify".into(),
                prompt: "TASK: verify the plan. DELIVERABLE: verification transcript. SCOPE: read-only. VERIFY: bun test. STOP WHEN: tests pass.".into(),
                label: None,
                category: Some("quick".into()),
                subagent_type: None,
                model: None,
                depends_on: Some(vec!["plan".into()]),
                task_summary: None,
                description: None,
                load_skills: None,
            },
        ],
    };
    let result = run_dag_tool(
        &deps(&f.manager),
        DagToolInput {
            action: "start".into(),
            definition: Some(compliant),
            run_id: None,
            reason: None,
            detach: None,
            node_id: None,
            node_ids: None,
            message: None,
            prompt: None,
        guidance_id: None,
        text: None,
        remove: None,
        },
    ).unwrap();
    let details = result.details.unwrap();
    assert_eq!(details["kind"], "started");
    assert!(details["warnings"].as_array().unwrap().is_empty());
}

#[test]
fn retired_momus_id_starts_as_ordinary_agent() {
    let f = fixture();
    let retired = DefinitionInput {
        key: "release-plan".into(),
        name: "release plan".into(),
        nodes: vec![NodeInput {
            id: "review".into(),
            prompt: "TASK: review the plan. DELIVERABLE: findings. SCOPE: read-only. VERIFY: findings listed. STOP WHEN: findings are written.".into(),
            label: None,
            category: None,
            subagent_type: Some("plan-reviewer".into()),
            model: Some("anthropic/claude-opus-4".into()),
            depends_on: None,
            task_summary: None,
            description: None,
            load_skills: None,
        }],
    };
    let result = run_dag_tool(
        &deps(&f.manager),
        DagToolInput {
            action: "start".into(),
            definition: Some(retired),
            run_id: None,
            reason: None,
            detach: None,
            node_id: None,
            node_ids: None,
            message: None,
            prompt: None,
        guidance_id: None,
        text: None,
        remove: None,
        },
    ).unwrap();
    let details = result.details.unwrap();
    assert_eq!(details["kind"], "started");
    let node = &details["snapshot"]["nodes"][0];
    assert_eq!(node["route"]["kind"], "agent");
    assert_eq!(node["route"]["agent"], "plan-reviewer");
    assert_eq!(node["route"]["model"], "anthropic/claude-opus-4");
    assert!(details["warnings"].as_array().unwrap().is_empty());
}

#[test]
fn retry_without_run_id_rejects() {
    let f = fixture();
    let retried = Arc::new(Mutex::new(Vec::new()));
    let retried_clone = retried.clone();
    let manager_clone = f.manager.clone();
    let with_retry = DagToolDeps {
        manager: f.manager.clone(),
        parent_session_id: Arc::new(|| PARENT_SESSION_ID.into()),
        root_session_id: Arc::new(|| ROOT_SESSION_ID.into()),
        wait: None,
        cancel: None,
        retry: Some(Arc::new(move |run: &str, _nodes: Option<&[String]>, _prompt: Option<&str>| {
            retried_clone.lock().unwrap().push(run.to_string());
            manager_clone.snapshot(&run.to_string(), PARENT_SESSION_ID).map_err(|e| ToolError::Message(e.to_string()).into())
        })),
        send: None,
        amend:None,

    cancel_node:None,

    guide:None,

    pause_reason:None,

    };

    let result = run_dag_tool(
        &with_retry,
        DagToolInput {
            action: "retry".into(),
            definition: None,
            run_id: None,
            reason: None,
            detach: None,
            node_id: None,
            node_ids: None,
            message: None,
            prompt: None,
        guidance_id: None,
        text: None,
        remove: None,
        },
    ).unwrap();
    let details = result.details.unwrap();
    assert_eq!(details["kind"], "error");
    assert_eq!(details["error"]["code"], "run_not_found");
    assert!(retried.lock().unwrap().is_empty());
}

#[test]
fn retry_with_both_node_id_and_node_ids_rejects() {
    let f = fixture();
    let started = run_dag_tool(
        &deps(&f.manager),
        DagToolInput {
            action: "start".into(),
            definition: Some(default_definition()),
            run_id: None,
            reason: None,
            detach: None,
            node_id: None,
            node_ids: None,
            message: None,
            prompt: None,
        guidance_id: None,
        text: None,
        remove: None,
        },
    ).unwrap();
    let run_id = started.details.unwrap()["run_id"].as_str().unwrap().to_string();

    let retried = Arc::new(Mutex::new(Vec::new()));
    let retried_clone = retried.clone();
    let manager_clone = f.manager.clone();
    let with_retry = DagToolDeps {
        manager: f.manager.clone(),
        parent_session_id: Arc::new(|| PARENT_SESSION_ID.into()),
        root_session_id: Arc::new(|| ROOT_SESSION_ID.into()),
        wait: None,
        cancel: None,
        retry: Some(Arc::new(move |run: &str, _nodes: Option<&[String]>, _prompt: Option<&str>| {
            retried_clone.lock().unwrap().push(run.to_string());
            manager_clone.snapshot(&run.to_string(), PARENT_SESSION_ID).map_err(|e| ToolError::Message(e.to_string()).into())
        })),
        send: None,
        amend:None,

    cancel_node:None,

    guide:None,

    pause_reason:None,

    };

    let result = run_dag_tool(
        &with_retry,
        DagToolInput {
            action: "retry".into(),
            definition: None,
            run_id: Some(run_id),
            reason: None,
            detach: None,
            node_id: Some("plan".into()),
            node_ids: Some(vec!["build".into()]),
            message: None,
            prompt: None,
        guidance_id: None,
        text: None,
        remove: None,
        },
    ).unwrap();
    let details = result.details.unwrap();
    assert_eq!(details["kind"], "error");
    assert_eq!(details["error"]["code"], "invalid_arguments");
    assert!(details["error"]["message"].as_str().unwrap().contains("node_ids"));
    assert!(retried.lock().unwrap().is_empty());
}

#[test]
fn send_without_message_rejects() {
    let f = fixture();
    let started = run_dag_tool(
        &deps(&f.manager),
        DagToolInput {
            action: "start".into(),
            definition: Some(default_definition()),
            run_id: None,
            reason: None,
            detach: None,
            node_id: None,
            node_ids: None,
            message: None,
            prompt: None,
        guidance_id: None,
        text: None,
        remove: None,
        },
    ).unwrap();
    let run_id = started.details.unwrap()["run_id"].as_str().unwrap().to_string();

    let sent = Arc::new(Mutex::new(Vec::new()));
    let sent_clone = sent.clone();
    let with_send = DagToolDeps {
        manager: f.manager.clone(),
        parent_session_id: Arc::new(|| PARENT_SESSION_ID.into()),
        root_session_id: Arc::new(|| ROOT_SESSION_ID.into()),
        wait: None,
        cancel: None,
        retry: None,
        send: Some(Arc::new(move |run: &str, node: &str, message: &str| {
            sent_clone.lock().unwrap().push(format!("{run}:{node}:{message}"));
            Ok(DagToolSendOutcome {
                node_id: node.to_string(),
                task_id: Some("task-1".into()),
                guidance_id: None,
                effect_id: None,
                delivery: DagToolSendDelivery::Steer,
                queue_position: None,
            })
        })),
        amend:None,

    cancel_node:None,

    guide:None,

    pause_reason:None,

    };

    let result = run_dag_tool(
        &with_send,
        DagToolInput {
            action: "send".into(),
            definition: None,
            run_id: Some(run_id),
            reason: None,
            detach: None,
            node_id: Some("plan".into()),
            node_ids: None,
            message: None,
            prompt: None,
        guidance_id: None,
        text: None,
        remove: None,
        },
    ).unwrap();
    let details = result.details.unwrap();
    assert_eq!(details["kind"], "error");
    assert_eq!(details["error"]["code"], "invalid_arguments");
    assert!(sent.lock().unwrap().is_empty());
}

#[test]
fn send_without_node_id_rejects() {
    let f = fixture();
    let started = run_dag_tool(
        &deps(&f.manager),
        DagToolInput {
            action: "start".into(),
            definition: Some(default_definition()),
            run_id: None,
            reason: None,
            detach: None,
            node_id: None,
            node_ids: None,
            message: None,
            prompt: None,
        guidance_id: None,
        text: None,
        remove: None,
        },
    ).unwrap();
    let run_id = started.details.unwrap()["run_id"].as_str().unwrap().to_string();

    let result = run_dag_tool(
        &deps(&f.manager),
        DagToolInput {
            action: "send".into(),
            definition: None,
            run_id: Some(run_id),
            reason: None,
            detach: None,
            node_id: None,
            node_ids: None,
            message: Some("keep going".into()),
            prompt: None,
        guidance_id: None,
        text: None,
        remove: None,
        },
    ).unwrap();
    let details = result.details.unwrap();
    assert_eq!(details["kind"], "error");
    assert_eq!(details["error"]["code"], "invalid_arguments");
}

#[test]
fn amend_without_definition_rejects() {
    let f = fixture();
    let started = run_dag_tool(
        &deps(&f.manager),
        DagToolInput {
            action: "start".into(),
            definition: Some(default_definition()),
            run_id: None,
            reason: None,
            detach: None,
            node_id: None,
            node_ids: None,
            message: None,
            prompt: None,
        guidance_id: None,
        text: None,
        remove: None,
        },
    ).unwrap();
    let run_id = started.details.unwrap()["run_id"].as_str().unwrap().to_string();

    let amended = Arc::new(Mutex::new(Vec::new()));
    let amended_clone = amended.clone();
    let manager_clone = f.manager.clone();
    let with_amend = DagToolDeps {
        cancel_node: None, guide: None, pause_reason: None,
        manager: f.manager.clone(),
        parent_session_id: Arc::new(|| PARENT_SESSION_ID.into()),
        root_session_id: Arc::new(|| ROOT_SESSION_ID.into()),
        wait: None,
        cancel: None,
        retry: None,
        send: None,
        amend: Some(Arc::new(move |run: &str, _def| {
            amended_clone.lock().unwrap().push(run.to_string());
            manager_clone.snapshot(&run.to_string(), PARENT_SESSION_ID).map_err(|e| ToolError::Message(e.to_string()).into())
        })),
    };

    let result = run_dag_tool(
        &with_amend,
        DagToolInput {
            action: "amend".into(),
            definition: None,
            run_id: Some(run_id),
            reason: None,
            detach: None,
            node_id: None,
            node_ids: None,
            message: None,
            prompt: None,
        guidance_id: None,
        text: None,
        remove: None,
        },
    ).unwrap();
    let details = result.details.unwrap();
    assert_eq!(details["kind"], "error");
    assert_eq!(details["error"]["code"], "invalid_arguments");
    assert!(amended.lock().unwrap().is_empty());
}

#[test]
fn retry_prompt_override_alongside_node_ids_rejects() {
    let f = fixture();
    let started = run_dag_tool(
        &deps(&f.manager),
        DagToolInput {
            action: "start".into(),
            definition: Some(default_definition()),
            run_id: None,
            reason: None,
            detach: None,
            node_id: None,
            node_ids: None,
            message: None,
            prompt: None,
        guidance_id: None,
        text: None,
        remove: None,
        },
    ).unwrap();
    let run_id = started.details.unwrap()["run_id"].as_str().unwrap().to_string();

    let result = run_dag_tool(
        &deps(&f.manager),
        DagToolInput {
            action: "retry".into(),
            definition: None,
            run_id: Some(run_id),
            reason: None,
            detach: None,
            node_id: None,
            node_ids: Some(vec!["plan".into(), "build".into()]),
            message: None,
            prompt: Some("try harder".into()),
        guidance_id: None,
        text: None,
        remove: None,
        },
    ).unwrap();
    let details = result.details.unwrap();
    assert_eq!(details["kind"], "error");
    assert_eq!(details["error"]["code"], "invalid_arguments");
    assert!(details["error"]["message"].as_str().unwrap().contains("prompt"));
}

#[test]
fn amend_definition_with_impossible_target_rejects_before_seam() {
    let f = fixture();
    let started = run_dag_tool(
        &deps(&f.manager),
        DagToolInput {
            action: "start".into(),
            definition: Some(default_definition()),
            run_id: None,
            reason: None,
            detach: None,
            node_id: None,
            node_ids: None,
            message: None,
            prompt: None,
        guidance_id: None,
        text: None,
        remove: None,
        },
    ).unwrap();
    let run_id = started.details.unwrap()["run_id"].as_str().unwrap().to_string();

    let amended = Arc::new(Mutex::new(Vec::new()));
    let amended_clone = amended.clone();
    let manager_clone = f.manager.clone();
    let with_amend = DagToolDeps {
        cancel_node: None, guide: None, pause_reason: None,
        manager: f.manager.clone(),
        parent_session_id: Arc::new(|| PARENT_SESSION_ID.into()),
        root_session_id: Arc::new(|| ROOT_SESSION_ID.into()),
        wait: None,
        cancel: None,
        retry: None,
        send: None,
        amend: Some(Arc::new(move |run: &str, _def| {
            amended_clone.lock().unwrap().push(run.to_string());
            manager_clone.snapshot(&run.to_string(), PARENT_SESSION_ID).map_err(|e| ToolError::Message(e.to_string()).into())
        })),
    };

    let invalid_def = DefinitionInput {
        key: "release-plan".into(),
        name: "release plan".into(),
        nodes: vec![NodeInput {
            id: "plan".into(),
            prompt: "draft".into(),
            label: None,
            category: Some("quick".into()),
            subagent_type: None,
            model: Some("anthropic/claude-opus-4".into()),
            depends_on: None,
            task_summary: None,
            description: None,
            load_skills: None,
        }],
    };

    let result = run_dag_tool(
        &with_amend,
        DagToolInput {
            action: "amend".into(),
            definition: Some(invalid_def),
            run_id: Some(run_id),
            reason: None,
            detach: None,
            node_id: None,
            node_ids: None,
            message: None,
            prompt: None,
        guidance_id: None,
        text: None,
        remove: None,
        },
    ).unwrap();
    let details = result.details.unwrap();
    assert_eq!(details["kind"], "error");
    assert_eq!(details["error"]["code"], "invalid_definition");
    assert_eq!(details["error"]["nodes"][0]["code"], "category_with_model");
    assert!(amended.lock().unwrap().is_empty());
}

#[test]
fn retry_seam_receives_targets_and_prompt() {
    let f = fixture();
    let started = run_dag_tool(
        &deps(&f.manager),
        DagToolInput {
            action: "start".into(),
            definition: Some(default_definition()),
            run_id: None,
            reason: None,
            detach: None,
            node_id: None,
            node_ids: None,
            message: None,
            prompt: None,
        guidance_id: None,
        text: None,
        remove: None,
        },
    ).unwrap();
    let run_id = started.details.unwrap()["run_id"].as_str().unwrap().to_string();

    let calls = Arc::new(Mutex::new(Vec::new()));
    let calls_clone = calls.clone();
    let manager_clone = f.manager.clone();
    let with_retry = DagToolDeps {
        manager: f.manager.clone(),
        parent_session_id: Arc::new(|| PARENT_SESSION_ID.into()),
        root_session_id: Arc::new(|| ROOT_SESSION_ID.into()),
        wait: None,
        cancel: None,
        retry: Some(Arc::new(move |run: &str, node_ids: Option<&[String]>, prompt: Option<&str>| {
            calls_clone.lock().unwrap().push((run.to_string(), node_ids.map(|v| v.to_vec()), prompt.map(String::from)));
            manager_clone.snapshot(&run.to_string(), PARENT_SESSION_ID).map_err(|e| ToolError::Message(e.to_string()).into())
        })),
        send: None,
        amend:None,

    cancel_node:None,

    guide:None,

    pause_reason:None,

    };

    let result = run_dag_tool(
        &with_retry,
        DagToolInput {
            action: "retry".into(),
            definition: None,
            run_id: Some(run_id.clone()),
            reason: None,
            detach: None,
            node_id: Some("plan".into()),
            node_ids: None,
            message: None,
            prompt: Some("try harder".into()),
        guidance_id: None,
        text: None,
        remove: None,
        },
    ).unwrap();
    let details = result.details.unwrap();
    assert_eq!(details["kind"], "retried");
    assert_eq!(details["run_id"], run_id);
    assert_eq!(details["node_ids"], json!(["plan"]));
    assert_eq!(*calls.lock().unwrap(), vec![(run_id, Some(vec!["plan".into()]), Some("try harder".into()))]);
}

#[test]
fn retry_with_neither_selector_passes_none() {
    let f = fixture();
    let started = run_dag_tool(
        &deps(&f.manager),
        DagToolInput {
            action: "start".into(),
            definition: Some(default_definition()),
            run_id: None,
            reason: None,
            detach: None,
            node_id: None,
            node_ids: None,
            message: None,
            prompt: None,
        guidance_id: None,
        text: None,
        remove: None,
        },
    ).unwrap();
    let run_id = started.details.unwrap()["run_id"].as_str().unwrap().to_string();

    let calls = Arc::new(Mutex::new(Vec::new()));
    let calls_clone = calls.clone();
    let manager_clone = f.manager.clone();
    let run_id_clone = run_id.clone();
    let with_retry = DagToolDeps {
        manager: f.manager.clone(),
        parent_session_id: Arc::new(|| PARENT_SESSION_ID.into()),
        root_session_id: Arc::new(|| ROOT_SESSION_ID.into()),
        wait: None,
        cancel: None,
        retry: Some(Arc::new(move |_run: &str, node_ids: Option<&[String]>, _prompt: Option<&str>| {
            calls_clone.lock().unwrap().push(node_ids.map(|v| v.to_vec()));
            manager_clone.snapshot(&run_id_clone, PARENT_SESSION_ID).map_err(|e| ToolError::Message(e.to_string()).into())
        })),
        send: None,
        amend:None,

    cancel_node:None,

    guide:None,

    pause_reason:None,

    };

    run_dag_tool(
        &with_retry,
        DagToolInput {
            action: "retry".into(),
            definition: None,
            run_id: Some(run_id),
            reason: None,
            detach: None,
            node_id: None,
            node_ids: None,
            message: None,
            prompt: None,
        guidance_id: None,
        text: None,
        remove: None,
        },
    ).unwrap();
    assert_eq!(*calls.lock().unwrap(), vec![None]);
}

#[test]
fn send_seam_returns_delivery_kind() {
    let f = fixture();
    let started = run_dag_tool(
        &deps(&f.manager),
        DagToolInput {
            action: "start".into(),
            definition: Some(default_definition()),
            run_id: None,
            reason: None,
            detach: None,
            node_id: None,
            node_ids: None,
            message: None,
            prompt: None,
        guidance_id: None,
        text: None,
        remove: None,
        },
    ).unwrap();
    let run_id = started.details.unwrap()["run_id"].as_str().unwrap().to_string();

    let with_send = DagToolDeps {
        manager: f.manager.clone(),
        parent_session_id: Arc::new(|| PARENT_SESSION_ID.into()),
        root_session_id: Arc::new(|| ROOT_SESSION_ID.into()),
        wait: None,
        cancel: None,
        retry: None,
        send: Some(Arc::new(move |_run: &str, node: &str, _msg: &str| {
            Ok(DagToolSendOutcome {
                node_id: node.to_string(),
                task_id: Some("task-7".into()),
                guidance_id: None,
                effect_id: None,
                delivery: DagToolSendDelivery::Revive,
                queue_position: None,
            })
        })),
        amend:None,

    cancel_node:None,

    guide:None,

    pause_reason:None,

    };

    let result = run_dag_tool(
        &with_send,
        DagToolInput {
            action: "send".into(),
            definition: None,
            run_id: Some(run_id),
            reason: None,
            detach: None,
            node_id: Some("plan".into()),
            node_ids: None,
            message: Some("resume with the new spec".into()),
            prompt: None,
        guidance_id: None,
        text: None,
        remove: None,
        },
    ).unwrap();
    let details = result.details.unwrap();
    assert_eq!(details["kind"], "sent");
    assert_eq!(details["node_id"], "plan");
    assert_eq!(details["delivery"], "revive");
    assert_eq!(details["task_id"], "task-7");
}

#[test]
fn amend_seam_receives_compiled_definition() {
    let f = fixture();
    let started = run_dag_tool(
        &deps(&f.manager),
        DagToolInput {
            action: "start".into(),
            definition: Some(default_definition()),
            run_id: None,
            reason: None,
            detach: None,
            node_id: None,
            node_ids: None,
            message: None,
            prompt: None,
        guidance_id: None,
        text: None,
        remove: None,
        },
    ).unwrap();
    let run_id = started.details.unwrap()["run_id"].as_str().unwrap().to_string();

    let amended = Arc::new(Mutex::new(Vec::new()));
    let amended_clone = amended.clone();
    let manager_clone = f.manager.clone();
    let with_amend = DagToolDeps {
        cancel_node: None, guide: None, pause_reason: None,
        manager: f.manager.clone(),
        parent_session_id: Arc::new(|| PARENT_SESSION_ID.into()),
        root_session_id: Arc::new(|| ROOT_SESSION_ID.into()),
        wait: None,
        cancel: None,
        retry: None,
        send: None,
        amend: Some(Arc::new(move |run: &str, def| {
            let prompts: Vec<String> = def.nodes.iter().map(|n| n.prompt.clone()).collect();
            amended_clone.lock().unwrap().push((run.to_string(), prompts));
            manager_clone.snapshot(&run.to_string(), PARENT_SESSION_ID).map_err(|e| ToolError::Message(e.to_string()).into())
        })),
    };

    let amended_def = DefinitionInput {
        key: "release-plan".into(),
        name: "release plan".into(),
        nodes: vec![
            NodeInput {
                id: "plan".into(),
                prompt: "draft a BETTER plan".into(),
                label: None,
                category: Some("quick".into()),
                subagent_type: None,
                model: None,
                depends_on: None,
                task_summary: None,
                description: None,
                load_skills: None,
            },
            NodeInput {
                id: "build".into(),
                prompt: "build it".into(),
                label: None,
                category: Some("quick".into()),
                subagent_type: None,
                model: None,
                depends_on: Some(vec!["plan".into()]),
                task_summary: None,
                description: None,
                load_skills: None,
            },
        ],
    };

    let result = run_dag_tool(
        &with_amend,
        DagToolInput {
            action: "amend".into(),
            definition: Some(amended_def),
            run_id: Some(run_id.clone()),
            reason: None,
            detach: None,
            node_id: None,
            node_ids: None,
            message: None,
            prompt: None,
        guidance_id: None,
        text: None,
        remove: None,
        },
    ).unwrap();
    let details = result.details.unwrap();
    assert_eq!(details["kind"], "amended");
    assert_eq!(details["run_id"], run_id);
    assert_eq!(
        *amended.lock().unwrap(),
        vec![(run_id, vec!["draft a BETTER plan".into(), "build it".into()])]
    );
}

#[test]
fn engine_refusal_node_not_found_surfaces_verbatim() {
    let f = fixture();
    let started = run_dag_tool(
        &deps(&f.manager),
        DagToolInput {
            action: "start".into(),
            definition: Some(default_definition()),
            run_id: None,
            reason: None,
            detach: None,
            node_id: None,
            node_ids: None,
            message: None,
            prompt: None,
        guidance_id: None,
        text: None,
        remove: None,
        },
    ).unwrap();
    let run_id = started.details.unwrap()["run_id"].as_str().unwrap().to_string();

    let with_error = DagToolDeps {
        manager: f.manager.clone(),
        parent_session_id: Arc::new(|| PARENT_SESSION_ID.into()),
        root_session_id: Arc::new(|| ROOT_SESSION_ID.into()),
        wait: None,
        cancel: None,
        retry: Some(Arc::new(move |_run, _nodes, _prompt| {
            let mut err = DagNodeControlError::new(
                DagNodeControlErrorCode::NodeNotFound,
                "unknown dag node \"ghost\"",
            );
            err.node_ids = vec!["plan".into()];
            Err(DagToolExecutionError::Control(err))
        })),
        send: None,
        amend:None,

    cancel_node:None,

    guide:None,

    pause_reason:None,

    };

    let result = run_dag_tool(
        &with_error,
        DagToolInput {
            action: "retry".into(),
            definition: None,
            run_id: Some(run_id),
            reason: None,
            detach: None,
            node_id: None,
            node_ids: None,
            message: None,
            prompt: None,
        guidance_id: None,
        text: None,
        remove: None,
        },
    ).unwrap();
    let details = result.details.unwrap();
    assert_eq!(details["kind"], "error");
    assert_eq!(details["error"]["code"], "node_not_found");
    assert_eq!(details["error"]["message"], "unknown dag node \"ghost\"");
    assert_eq!(details["error"]["node_ids"], json!(["plan"]));
}

#[test]
fn engine_refusal_node_not_retryable_surfaces_verbatim() {
    let f = fixture();
    let started = run_dag_tool(
        &deps(&f.manager),
        DagToolInput {
            action: "start".into(),
            definition: Some(default_definition()),
            run_id: None,
            reason: None,
            detach: None,
            node_id: None,
            node_ids: None,
            message: None,
            prompt: None,
        guidance_id: None,
        text: None,
        remove: None,
        },
    ).unwrap();
    let run_id = started.details.unwrap()["run_id"].as_str().unwrap().to_string();

    let with_error = DagToolDeps {
        manager: f.manager.clone(),
        parent_session_id: Arc::new(|| PARENT_SESSION_ID.into()),
        root_session_id: Arc::new(|| ROOT_SESSION_ID.into()),
        wait: None,
        cancel: None,
        retry: Some(Arc::new(move |_run, _nodes, _prompt| {
            let mut err = DagNodeControlError::new(
                DagNodeControlErrorCode::NodeNotRetryable,
                "dag node \"plan\" already completed",
            );
            err.node_ids = vec!["plan".into()];
            Err(DagToolExecutionError::Control(err))
        })),
        send: None,
        amend:None,

    cancel_node:None,

    guide:None,

    pause_reason:None,

    };

    let result = run_dag_tool(
        &with_error,
        DagToolInput {
            action: "retry".into(),
            definition: None,
            run_id: Some(run_id),
            reason: None,
            detach: None,
            node_id: None,
            node_ids: None,
            message: None,
            prompt: None,
        guidance_id: None,
        text: None,
        remove: None,
        },
    ).unwrap();
    let details = result.details.unwrap();
    assert_eq!(details["kind"], "error");
    assert_eq!(details["error"]["code"], "node_not_retryable");
    assert_eq!(details["error"]["message"], "dag node \"plan\" already completed");
    assert_eq!(details["error"]["node_ids"], json!(["plan"]));
}

#[test]
fn engine_refusal_node_not_continuable_surfaces_verbatim() {
    let f = fixture();
    let started = run_dag_tool(
        &deps(&f.manager),
        DagToolInput {
            action: "start".into(),
            definition: Some(default_definition()),
            run_id: None,
            reason: None,
            detach: None,
            node_id: None,
            node_ids: None,
            message: None,
            prompt: None,
        guidance_id: None,
        text: None,
        remove: None,
        },
    ).unwrap();
    let run_id = started.details.unwrap()["run_id"].as_str().unwrap().to_string();

    let with_error = DagToolDeps {
        manager: f.manager.clone(),
        parent_session_id: Arc::new(|| PARENT_SESSION_ID.into()),
        root_session_id: Arc::new(|| ROOT_SESSION_ID.into()),
        wait: None,
        cancel: None,
        retry: Some(Arc::new(move |_run, _nodes, _prompt| {
            let mut err = DagNodeControlError::new(
                DagNodeControlErrorCode::NodeNotContinuable,
                "dag node \"plan\" cannot be continued",
            );
            err.node_ids = vec!["plan".into()];
            Err(DagToolExecutionError::Control(err))
        })),
        send: None,
        amend:None,

    cancel_node:None,

    guide:None,

    pause_reason:None,

    };

    let result = run_dag_tool(
        &with_error,
        DagToolInput {
            action: "retry".into(),
            definition: None,
            run_id: Some(run_id),
            reason: None,
            detach: None,
            node_id: None,
            node_ids: None,
            message: None,
            prompt: None,
        guidance_id: None,
        text: None,
        remove: None,
        },
    ).unwrap();
    let details = result.details.unwrap();
    assert_eq!(details["kind"], "error");
    assert_eq!(details["error"]["code"], "node_not_continuable");
    assert_eq!(details["error"]["message"], "dag node \"plan\" cannot be continued");
    assert_eq!(details["error"]["node_ids"], json!(["plan"]));
}

#[test]
fn engine_refusal_run_still_active_surfaces_verbatim() {
    let f = fixture();
    let started = run_dag_tool(
        &deps(&f.manager),
        DagToolInput {
            action: "start".into(),
            definition: Some(default_definition()),
            run_id: None,
            reason: None,
            detach: None,
            node_id: None,
            node_ids: None,
            message: None,
            prompt: None,
        guidance_id: None,
        text: None,
        remove: None,
        },
    ).unwrap();
    let run_id = started.details.unwrap()["run_id"].as_str().unwrap().to_string();

    let with_error = DagToolDeps {
        manager: f.manager.clone(),
        parent_session_id: Arc::new(|| PARENT_SESSION_ID.into()),
        root_session_id: Arc::new(|| ROOT_SESSION_ID.into()),
        wait: None,
        cancel: None,
        retry: Some(Arc::new(move |_run, _nodes, _prompt| {
            let mut err = DagNodeControlError::new(
                DagNodeControlErrorCode::RunStillActive,
                "run is still active",
            );
            err.node_ids = vec!["plan".into()];
            Err(DagToolExecutionError::Control(err))
        })),
        send: None,
        amend:None,

    cancel_node:None,

    guide:None,

    pause_reason:None,

    };

    let result = run_dag_tool(
        &with_error,
        DagToolInput {
            action: "retry".into(),
            definition: None,
            run_id: Some(run_id),
            reason: None,
            detach: None,
            node_id: None,
            node_ids: None,
            message: None,
            prompt: None,
        guidance_id: None,
        text: None,
        remove: None,
        },
    ).unwrap();
    let details = result.details.unwrap();
    assert_eq!(details["kind"], "error");
    assert_eq!(details["error"]["code"], "run_still_active");
    assert_eq!(details["error"]["message"], "run is still active");
    assert_eq!(details["error"]["node_ids"], json!(["plan"]));
}

#[test]
fn engine_refusal_invalid_arguments_surfaces_verbatim() {
    let f = fixture();
    let started = run_dag_tool(
        &deps(&f.manager),
        DagToolInput {
            action: "start".into(),
            definition: Some(default_definition()),
            run_id: None,
            reason: None,
            detach: None,
            node_id: None,
            node_ids: None,
            message: None,
            prompt: None,
        guidance_id: None,
        text: None,
        remove: None,
        },
    ).unwrap();
    let run_id = started.details.unwrap()["run_id"].as_str().unwrap().to_string();

    let with_error = DagToolDeps {
        manager: f.manager.clone(),
        parent_session_id: Arc::new(|| PARENT_SESSION_ID.into()),
        root_session_id: Arc::new(|| ROOT_SESSION_ID.into()),
        wait: None,
        cancel: None,
        retry: Some(Arc::new(move |_run, _nodes, _prompt| {
            let mut err = DagNodeControlError::new(
                DagNodeControlErrorCode::InvalidArguments,
                "a retry prompt override requires exactly one explicit node id",
            );
            err.node_ids = vec!["plan".into()];
            Err(DagToolExecutionError::Control(err))
        })),
        send: None,
        amend:None,

    cancel_node:None,

    guide:None,

    pause_reason:None,

    };

    let result = run_dag_tool(
        &with_error,
        DagToolInput {
            action: "retry".into(),
            definition: None,
            run_id: Some(run_id),
            reason: None,
            detach: None,
            node_id: None,
            node_ids: None,
            message: None,
            prompt: None,
        guidance_id: None,
        text: None,
        remove: None,
        },
    ).unwrap();
    let details = result.details.unwrap();
    assert_eq!(details["kind"], "error");
    assert_eq!(details["error"]["code"], "invalid_arguments");
    assert_eq!(details["error"]["message"], "a retry prompt override requires exactly one explicit node id");
    assert_eq!(details["error"]["node_ids"], json!(["plan"]));
}

#[test]
fn amend_refusal_invalid_amendment_surfaces() {
    let f = fixture();
    let started = run_dag_tool(
        &deps(&f.manager),
        DagToolInput {
            action: "start".into(),
            definition: Some(default_definition()),
            run_id: None,
            reason: None,
            detach: None,
            node_id: None,
            node_ids: None,
            message: None,
            prompt: None,
        guidance_id: None,
        text: None,
        remove: None,
        },
    ).unwrap();
    let run_id = started.details.unwrap()["run_id"].as_str().unwrap().to_string();

    let with_error = DagToolDeps {
        cancel_node: None, guide: None, pause_reason: None,
        manager: f.manager.clone(),
        parent_session_id: Arc::new(|| PARENT_SESSION_ID.into()),
        root_session_id: Arc::new(|| ROOT_SESSION_ID.into()),
        wait: None,
        cancel: None,
        retry: None,
        send: None,
        amend: Some(Arc::new(move |_run, _def| {
            let err = DagManagerError::new(
                DagManagerErrorCode::InvalidAmendment,
                "removed node still has dependents",
            );
            Err(DagToolExecutionError::Manager(err))
        })),
    };

    let result = run_dag_tool(
        &with_error,
        DagToolInput {
            action: "amend".into(),
            definition: Some(default_definition()),
            run_id: Some(run_id),
            reason: None,
            detach: None,
            node_id: None,
            node_ids: None,
            message: None,
            prompt: None,
        guidance_id: None,
        text: None,
        remove: None,
        },
    ).unwrap();
    let details = result.details.unwrap();
    assert_eq!(details["kind"], "error");
    assert_eq!(details["error"]["code"], "invalid_amendment");
    assert_eq!(details["error"]["message"], "removed node still has dependents");
}

#[test]
fn amend_refusal_amend_running_node_surfaces() {
    let f = fixture();
    let started = run_dag_tool(
        &deps(&f.manager),
        DagToolInput {
            action: "start".into(),
            definition: Some(default_definition()),
            run_id: None,
            reason: None,
            detach: None,
            node_id: None,
            node_ids: None,
            message: None,
            prompt: None,
        guidance_id: None,
        text: None,
        remove: None,
        },
    ).unwrap();
    let run_id = started.details.unwrap()["run_id"].as_str().unwrap().to_string();

    let with_error = DagToolDeps {
        cancel_node: None, guide: None, pause_reason: None,
        manager: f.manager.clone(),
        parent_session_id: Arc::new(|| PARENT_SESSION_ID.into()),
        root_session_id: Arc::new(|| ROOT_SESSION_ID.into()),
        wait: None,
        cancel: None,
        retry: None,
        send: None,
        amend: Some(Arc::new(move |_run, _def| {
            let err = DagManagerError::new(
                DagManagerErrorCode::AmendRunningNode,
                "cannot amend scheduled or running nodes: plan",
            );
            Err(DagToolExecutionError::Manager(err))
        })),
    };

    let result = run_dag_tool(
        &with_error,
        DagToolInput {
            action: "amend".into(),
            definition: Some(default_definition()),
            run_id: Some(run_id),
            reason: None,
            detach: None,
            node_id: None,
            node_ids: None,
            message: None,
            prompt: None,
        guidance_id: None,
        text: None,
        remove: None,
        },
    ).unwrap();
    let details = result.details.unwrap();
    assert_eq!(details["kind"], "error");
    assert_eq!(details["error"]["code"], "amend_running_node");
    assert_eq!(details["error"]["message"], "cannot amend scheduled or running nodes: plan");
}

#[test]
fn retry_foreign_session_enforces_run_not_owned() {
    let f = fixture();
    let started = run_dag_tool(
        &deps(&f.manager),
        DagToolInput {
            action: "start".into(),
            definition: Some(default_definition()),
            run_id: None,
            reason: None,
            detach: None,
            node_id: None,
            node_ids: None,
            message: None,
            prompt: None,
        guidance_id: None,
        text: None,
        remove: None,
        },
    ).unwrap();
    let run_id = started.details.unwrap()["run_id"].as_str().unwrap().to_string();

    let retried = Arc::new(Mutex::new(Vec::new()));
    let retried_clone = retried.clone();
    let manager_clone = f.manager.clone();
    let foreign = DagToolDeps {
        manager: f.manager.clone(),
        parent_session_id: Arc::new(|| FOREIGN_SESSION_ID.into()),
        root_session_id: Arc::new(|| ROOT_SESSION_ID.into()),
        wait: None,
        cancel: None,
        retry: Some(Arc::new(move |run: &str, _nodes: Option<&[String]>, _prompt: Option<&str>| {
            retried_clone.lock().unwrap().push(run.to_string());
            manager_clone.snapshot(&run.to_string(), FOREIGN_SESSION_ID).map_err(|e| ToolError::Message(e.to_string()).into())
        })),
        send: None,
        amend:None,

    cancel_node:None,

    guide:None,

    pause_reason:None,

    };

    let result = run_dag_tool(
        &foreign,
        DagToolInput {
            action: "retry".into(),
            definition: None,
            run_id: Some(run_id),
            reason: None,
            detach: None,
            node_id: None,
            node_ids: None,
            message: None,
            prompt: None,
        guidance_id: None,
        text: None,
        remove: None,
        },
    ).unwrap();
    let details = result.details.unwrap();
    assert_eq!(details["kind"], "error");
    assert_eq!(details["error"]["code"], "run_not_owned");
    assert!(retried.lock().unwrap().is_empty());
}

#[test]
fn workflow_tool_schema_offers_control_verbs() {
    let f = fixture();
    let tool = create_dag_tool(deps(&f.manager));
    let action_schema = &tool.parameters["properties"]["action"];
    let members: Vec<&str> = action_schema["anyOf"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["const"].as_str().unwrap())
        .collect();
    assert_eq!(
        members,
        vec!["start", "attach", "snapshot", "wait", "cancel", "retry", "send", "amend", "resume", "cancel_node", "guide"]
    );
    let desc = action_schema["description"].as_str().unwrap();
    assert!(desc.contains("retry"));
    assert!(desc.contains("send"));
    assert!(desc.contains("amend"));
}

#[tokio::test(flavor = "current_thread")]
async fn blocking_wait_does_not_starve_the_executor_that_drives_the_run() {
    let f = fixture();
    let started = run_dag_tool(&deps(&f.manager), DagToolInput {
        action: "start".into(), definition: Some(default_definition()), run_id: None,
        reason: None, detach: None, node_id: None, node_ids: None, message: None, prompt: None,
    guidance_id: None,
    text: None,
    remove: None,
    }).expect("start");
    let run_id = started.details.expect("details")["run_id"].as_str().expect("run id").to_owned();
    // The run settles only when a task on this same current-thread executor makes progress, the way
    // in-process children stream their provider responses on the CLI runtime.
    let (settle, settled) = std::sync::mpsc::channel::<()>();
    let settled = Mutex::new(settled);
    let mut dependencies = deps(&f.manager);
    dependencies.wait = Some(Arc::new(move |run, _| {
        settled.lock().expect("settle receiver").recv_timeout(std::time::Duration::from_secs(10)).map_err(|_| ToolError::Message("executor starved".into()))?;
        Ok(json!({"runId": run, "status": "completed"}))
    }));
    tokio::spawn(async move { settle.send(()).expect("waiter alive") });
    let tool = create_dag_tool(dependencies);
    let result = (tool.execute)(maho_ext_api::ToolCall {
        id: "wait", params: json!({"action": "wait", "run_id": run_id, "detach": false}),
        signal: Default::default(), on_update: None, context: None,
    }).await.expect("wait settles while the executor keeps running");
    assert_eq!(result.details.expect("details")["result"]["status"], "completed");
}
