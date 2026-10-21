//! Port of upstream `prompt.test.ts` with real GatewayComponent.register and BeforeAgentStart event dispatch.

mod support;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use rusqlite::Connection;
use serde_json::json;
use tempfile::tempdir;

use maho_ext_api::{
    BeforeAgentStartEvent, BuildSystemPromptOptions, EventKind, EventResult, Extension,
    ExtensionEvent,
};
use maho_omo_gateway::{
    apply_gateway_rules_migrations, gateway_database_path, rules_committed, GatewayComponent,
    GatewayComponentOptions, GatewayRulesPromptHandler, GatewayRulesPromptOptions,
    GatewayRulesStore, RulesCommittedArgs, RulesCommittedTarget, SqliteGatewayRulesStore,
    DeliverySummary, EnqueueDeliveryRequest, GatewayDeliveryEnqueueSeam,
};

use support::{make_test_api, test_context};

struct MockEnqueueSeam;
impl GatewayDeliveryEnqueueSeam for MockEnqueueSeam {
    fn enqueue(
        &self,
        _tx: &rusqlite::Transaction<'_>,
        request: &EnqueueDeliveryRequest,
        _now: f64,
    ) -> Result<DeliverySummary, String> {
        Ok(DeliverySummary::Ok {
            delivery_id: format!("deliv_{}", request.event_id),
            deduplicated: false,
        })
    }
}

fn make_before_agent_start_event(system_prompt: &str) -> ExtensionEvent {
    ExtensionEvent::BeforeAgentStart(BeforeAgentStartEvent {
        prompt: "hello".into(),
        trigger: "prompt".into(),
        preview: false,
        images: None,
        system_prompt: system_prompt.into(),
        system_prompt_options: BuildSystemPromptOptions::default(),
    })
}

#[tokio::test]
async fn given_a_session_with_no_committed_rules_when_the_handler_runs_then_it_returns_none() {
    let mut conn = Connection::open_in_memory().unwrap();
    apply_gateway_rules_migrations(&mut conn).unwrap();
    let store: Arc<dyn GatewayRulesStore> = Arc::new(SqliteGatewayRulesStore::new(conn));

    let handler = GatewayRulesPromptHandler::new(GatewayRulesPromptOptions {
        ensure_store: Arc::new(move || Ok(Some(store.clone()))),
        on_lookup_error: None,
    });

    assert_eq!(handler.handle("BASE PROMPT", "unbound-session"), None);
}

#[tokio::test]
async fn given_a_session_with_committed_rules_when_the_handler_runs_then_prompt_gains_operating_rules_block() {
    let mut conn = Connection::open_in_memory().unwrap();
    apply_gateway_rules_migrations(&mut conn).unwrap();
    let seam = MockEnqueueSeam;

    rules_committed(
        &mut conn,
        &RulesCommittedArgs {
            scope: "team".to_string(),
            version: "v1".to_string(),
            now: 1000.0,
            targets: vec![RulesCommittedTarget {
                session_durable_id: "sess-1".to_string(),
                binding_id: Some("b1".to_string()),
                behavioral: Some(vec!["answer in bullet points".to_string()]),
            }],
        },
        &seam,
    )
    .unwrap();

    let store: Arc<dyn GatewayRulesStore> = Arc::new(SqliteGatewayRulesStore::new(conn));
    let handler = GatewayRulesPromptHandler::new(GatewayRulesPromptOptions {
        ensure_store: Arc::new(move || Ok(Some(store.clone()))),
        on_lookup_error: None,
    });

    let res = handler.handle("BASE PROMPT", "sess-1").unwrap();
    assert!(res.system_prompt.starts_with("BASE PROMPT\n\n"));
    assert!(res.system_prompt.contains(r#"<operating-rules version="v1">"#));
    assert!(res.system_prompt.contains("- answer in bullet points"));
}

#[tokio::test]
async fn given_an_empty_system_prompt_when_the_handler_runs_then_it_composes_rules_block() {
    let mut conn = Connection::open_in_memory().unwrap();
    apply_gateway_rules_migrations(&mut conn).unwrap();
    let seam = MockEnqueueSeam;

    rules_committed(
        &mut conn,
        &RulesCommittedArgs {
            scope: "team".to_string(),
            version: "v1".to_string(),
            now: 1000.0,
            targets: vec![RulesCommittedTarget {
                session_durable_id: "sess-1".to_string(),
                binding_id: Some("b1".to_string()),
                behavioral: Some(vec!["rule one".to_string()]),
            }],
        },
        &seam,
    )
    .unwrap();

    let store: Arc<dyn GatewayRulesStore> = Arc::new(SqliteGatewayRulesStore::new(conn));
    let handler = GatewayRulesPromptHandler::new(GatewayRulesPromptOptions {
        ensure_store: Arc::new(move || Ok(Some(store.clone()))),
        on_lookup_error: None,
    });

    let res = handler.handle("", "sess-1").unwrap();
    assert!(res.system_prompt.contains(r#"<operating-rules version="v1">"#));
}

#[tokio::test]
async fn given_committed_rules_when_the_handler_runs_twice_on_same_version_then_both_prompts_are_byte_identical() {
    let mut conn = Connection::open_in_memory().unwrap();
    apply_gateway_rules_migrations(&mut conn).unwrap();
    let seam = MockEnqueueSeam;

    rules_committed(
        &mut conn,
        &RulesCommittedArgs {
            scope: "team".to_string(),
            version: "v1".to_string(),
            now: 1000.0,
            targets: vec![RulesCommittedTarget {
                session_durable_id: "sess-1".to_string(),
                binding_id: Some("b1".to_string()),
                behavioral: Some(vec!["rule one".to_string()]),
            }],
        },
        &seam,
    )
    .unwrap();

    let store: Arc<dyn GatewayRulesStore> = Arc::new(SqliteGatewayRulesStore::new(conn));
    let handler = GatewayRulesPromptHandler::new(GatewayRulesPromptOptions {
        ensure_store: Arc::new(move || Ok(Some(store.clone()))),
        on_lookup_error: None,
    });

    let first = handler.handle("BASE PROMPT", "sess-1").unwrap();
    let second = handler.handle(&first.system_prompt, "sess-1").unwrap();

    assert_eq!(second.system_prompt, first.system_prompt);
}

#[tokio::test]
async fn given_a_new_rules_version_when_the_handler_runs_again_then_the_block_is_replaced_not_appended() {
    let mut conn = Connection::open_in_memory().unwrap();
    apply_gateway_rules_migrations(&mut conn).unwrap();
    let seam = MockEnqueueSeam;

    rules_committed(
        &mut conn,
        &RulesCommittedArgs {
            scope: "team".to_string(),
            version: "v1".to_string(),
            now: 1000.0,
            targets: vec![RulesCommittedTarget {
                session_durable_id: "sess-1".to_string(),
                binding_id: Some("b1".to_string()),
                behavioral: Some(vec!["rule one".to_string()]),
            }],
        },
        &seam,
    )
    .unwrap();

    let conn_cell = Arc::new(std::sync::Mutex::new(conn));
    let conn_for_store = conn_cell.clone();

    struct SharedConnStore(Arc<std::sync::Mutex<Connection>>);
    impl GatewayRulesStore for SharedConnStore {
        fn block_for_session(&self, id: &str) -> Result<Option<maho_omo_gateway::SessionRulesBlock>, String> {
            let guard = self.0.lock().map_err(|e| e.to_string())?;
            maho_omo_gateway::block_for_session(&guard, id).map_err(|e| e.to_string())
        }
    }

    let store: Arc<dyn GatewayRulesStore> = Arc::new(SharedConnStore(conn_for_store));
    let handler = GatewayRulesPromptHandler::new(GatewayRulesPromptOptions {
        ensure_store: Arc::new(move || Ok(Some(store.clone()))),
        on_lookup_error: None,
    });

    let first = handler.handle("BASE PROMPT", "sess-1").unwrap();

    // Commit v2
    {
        let mut guard = conn_cell.lock().unwrap();
        rules_committed(
            &mut guard,
            &RulesCommittedArgs {
                scope: "team".to_string(),
                version: "v2".to_string(),
                now: 2000.0,
                targets: vec![RulesCommittedTarget {
                    session_durable_id: "sess-1".to_string(),
                    binding_id: Some("b1".to_string()),
                    behavioral: Some(vec!["rule two".to_string()]),
                }],
            },
            &seam,
        )
        .unwrap();
    }

    let second = handler.handle(&first.system_prompt, "sess-1").unwrap();
    assert!(second.system_prompt.contains(r#"version="v2""#));
    assert!(!second.system_prompt.contains(r#"version="v1""#));
    assert_eq!(
        second.system_prompt.matches("<!-- omo-gateway:rules:begin -->").count(),
        1
    );
}

#[tokio::test]
async fn given_no_gateway_store_when_the_handler_runs_then_it_returns_none() {
    let handler = GatewayRulesPromptHandler::new(GatewayRulesPromptOptions {
        ensure_store: Arc::new(|| Ok(None)),
        on_lookup_error: None,
    });

    assert_eq!(handler.handle("BASE PROMPT", "sess-1"), None);
}

#[tokio::test]
async fn given_a_lookup_failure_when_the_handler_runs_then_it_passes_through_and_warns_once() {
    struct FailingStore;
    impl GatewayRulesStore for FailingStore {
        fn block_for_session(&self, _id: &str) -> Result<Option<maho_omo_gateway::SessionRulesBlock>, String> {
            Err("worker died".to_string())
        }
    }

    let store: Arc<dyn GatewayRulesStore> = Arc::new(FailingStore);
    let warnings = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let warn_sink = warnings.clone();

    let handler = GatewayRulesPromptHandler::new(GatewayRulesPromptOptions {
        ensure_store: Arc::new(move || Ok(Some(store.clone()))),
        on_lookup_error: Some(Arc::new(move |msg| {
            warn_sink.lock().unwrap().push(msg.to_string());
        })),
    });

    assert_eq!(handler.handle("BASE PROMPT", "sess-1"), None);
    assert_eq!(handler.handle("BASE PROMPT", "sess-1"), None);

    let recorded = warnings.lock().unwrap();
    assert_eq!(recorded.len(), 1);
    assert!(recorded[0].contains("worker died"));
}

#[tokio::test]
async fn given_no_gateway_config_when_registered_component_receives_before_agent_start_then_it_returns_none_and_never_opens_store() {
    let dir = tempdir().unwrap();
    let agent_dir = dir.path().to_path_buf();
    let store_creations = Arc::new(AtomicUsize::new(0));
    let store_creations_clone = store_creations.clone();

    let component = GatewayComponent::new(GatewayComponentOptions {
        load_gateway_section: Some(Arc::new(|| None)),
        agent_dir: Some(Arc::new({
            let path = agent_dir.clone();
            move || path.clone()
        })),
        create_store: Some(Arc::new(move |_| {
            store_creations_clone.fetch_add(1, Ordering::SeqCst);
            panic!("store must not be opened without gateway config");
        })),
    });

    let mut api = make_test_api(agent_dir.clone());
    component.register(&mut api);

    let ctx = test_context("sess-1", agent_dir);
    let mut event = make_before_agent_start_event("BASE PROMPT");

    let handler = &api.registered.handlers[&EventKind::BeforeAgentStart][0];
    let result = handler(&mut event, &ctx).await.unwrap();

    assert!(matches!(result, EventResult::None));
    assert_eq!(store_creations.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn given_gateway_scopes_but_no_store_database_when_registered_component_receives_before_agent_start_then_it_returns_none_and_never_opens_store() {
    let empty_dir = tempdir().unwrap();
    let agent_dir = empty_dir.path().to_path_buf();
    let store_creations = Arc::new(AtomicUsize::new(0));
    let store_creations_clone = store_creations.clone();

    let component = GatewayComponent::new(GatewayComponentOptions {
        load_gateway_section: Some(Arc::new(|| Some(json!({"scopes": [{"id": "team"}]})))),
        agent_dir: Some(Arc::new({
            let path = agent_dir.clone();
            move || path.clone()
        })),
        create_store: Some(Arc::new(move |_| {
            store_creations_clone.fetch_add(1, Ordering::SeqCst);
            panic!("store must not be opened when database does not exist");
        })),
    });

    let mut api = make_test_api(agent_dir.clone());
    component.register(&mut api);

    let ctx = test_context("sess-1", agent_dir);
    let mut event = make_before_agent_start_event("BASE PROMPT");

    let handler = &api.registered.handlers[&EventKind::BeforeAgentStart][0];
    let result = handler(&mut event, &ctx).await.unwrap();

    assert!(matches!(result, EventResult::None));
    assert_eq!(store_creations.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn given_configured_gateway_and_existing_db_when_component_runs_then_rules_block_injected_for_bound_and_none_for_unbound() {
    let dir = tempdir().unwrap();
    let agent_dir = dir.path().to_path_buf();
    let db_path = gateway_database_path(&agent_dir);
    std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();

    // Populate database
    {
        let mut conn = Connection::open(&db_path).unwrap();
        apply_gateway_rules_migrations(&mut conn).unwrap();
        let seam = MockEnqueueSeam;
        rules_committed(
            &mut conn,
            &RulesCommittedArgs {
                scope: "team".to_string(),
                version: "v1".to_string(),
                now: 1000.0,
                targets: vec![RulesCommittedTarget {
                    session_durable_id: "bound-session".to_string(),
                    binding_id: Some("b1".to_string()),
                    behavioral: Some(vec!["answer in bullet points".to_string()]),
                }],
            },
            &seam,
        )
        .unwrap();
    }

    let component = GatewayComponent::new(GatewayComponentOptions {
        load_gateway_section: Some(Arc::new(|| Some(json!({"scopes": [{"id": "team"}]})))),
        agent_dir: Some(Arc::new({
            let path = agent_dir.clone();
            move || path.clone()
        })),
        create_store: None, // uses default_create_store
    });

    let mut api = make_test_api(agent_dir.clone());
    component.register(&mut api);

    let handler = &api.registered.handlers[&EventKind::BeforeAgentStart][0];

    // 1. Bound session receives operating-rules block
    let bound_ctx = test_context("bound-session", agent_dir.clone());
    let mut bound_event = make_before_agent_start_event("BASE PROMPT");
    let bound_result = handler(&mut bound_event, &bound_ctx).await.unwrap();

    match bound_result {
        EventResult::BeforeAgentStart(res) => {
            let prompt = res.system_prompt.unwrap();
            assert!(prompt.contains(r#"<operating-rules version="v1">"#));
            assert!(prompt.contains("- answer in bullet points"));
        }
        _ => panic!("expected BeforeAgentStart event result"),
    }

    // 2. Unbound session returns None (passes through unchanged)
    let unbound_ctx = test_context("unbound-session", agent_dir);
    let mut unbound_event = make_before_agent_start_event("BASE PROMPT");
    let unbound_result = handler(&mut unbound_event, &unbound_ctx).await.unwrap();
    assert!(matches!(unbound_result, EventResult::None));
}

#[tokio::test]
async fn given_default_component_lifecycle_across_restarts_then_populated_database_is_read_successfully() {
    let dir = tempdir().unwrap();
    let agent_dir = dir.path().to_path_buf();
    let db_path = gateway_database_path(&agent_dir);
    std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();

    // Turn 1: create DB, apply migrations, commit rules
    {
        let mut conn = Connection::open(&db_path).unwrap();
        apply_gateway_rules_migrations(&mut conn).unwrap();
        let seam = MockEnqueueSeam;
        rules_committed(
            &mut conn,
            &RulesCommittedArgs {
                scope: "team".to_string(),
                version: "v1".to_string(),
                now: 1000.0,
                targets: vec![RulesCommittedTarget {
                    session_durable_id: "sess-turn1".to_string(),
                    binding_id: Some("b-1".to_string()),
                    behavioral: Some(vec!["persisted across restarts".to_string()]),
                }],
            },
            &seam,
        )
        .unwrap();
    }

    // Turn 2: fresh component initialization on same database using default_create_store
    let component = GatewayComponent::new(GatewayComponentOptions {
        load_gateway_section: Some(Arc::new(|| Some(json!({"scopes": [{"id": "team"}]})))),
        agent_dir: Some(Arc::new({
            let path = agent_dir.clone();
            move || path.clone()
        })),
        create_store: None, // Uses default_create_store
    });

    let mut api = make_test_api(agent_dir.clone());
    component.register(&mut api);

    let handler = &api.registered.handlers[&EventKind::BeforeAgentStart][0];
    let ctx = test_context("sess-turn1", agent_dir);
    let mut event = make_before_agent_start_event("BASE PROMPT");

    let result = handler(&mut event, &ctx).await.unwrap();
    match result {
        EventResult::BeforeAgentStart(res) => {
            let prompt = res.system_prompt.unwrap();
            assert!(prompt.contains(r#"version="v1""#));
            assert!(prompt.contains("persisted across restarts"));
        }
        _ => panic!("expected BeforeAgentStart event result"),
    }
}
