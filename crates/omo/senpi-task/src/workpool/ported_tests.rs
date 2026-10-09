//! Ports of the workpool manager, tool, integration, and admission tests.

use serde_json::json;

use super::process_worker::register_process_workpool_worker;
use super::schema::{canonical_json, nonempty, parse_pool_id};
use super::store::WorkpoolStore;
use super::default_mode;
use super::types::{WorkpoolAgent, WorkpoolCaller, WorkpoolCreate, WorkpoolErrorCode, WorkpoolItem, WorkpoolItemStatus, WorkpoolMode, WorkpoolRecord, WorkpoolSpec, WorkpoolStatus};
use super::worker_capability::create_workpool_yield_capability;
use super::worker_kernel_tools::{KernelToolCapability, resolve_pool_kernel_tools};
use crate::manager::concurrency::{TaskConcurrency, TaskConcurrencyConfig};
use crate::team::member_extensions::assemble_member_extensions;

fn caller() -> WorkpoolCaller {
    WorkpoolCaller { session_id: "session".into(), root_session_id: "root".into(), depth: 0, cwd: "/tmp".into() }
}

fn input() -> WorkpoolCreate {
    WorkpoolCreate { name: "pool".into(), agent: WorkpoolAgent::Category { prompt: "run".into(), model: None, category: "quick".into() }, mode: Some(WorkpoolMode::Fresh), tools: None }
}

fn spec() -> WorkpoolSpec { WorkpoolSpec { start: Default::default(), plan: Default::default() } }

fn record() -> WorkpoolRecord {
    WorkpoolRecord { version: 1, pool_id: "wp_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(), name: "pool".into(), parent_session_id: "session".into(), root_session_id: "root".into(), generation: 1, revision: 0, mode: WorkpoolMode::Fresh, agent: input().agent, worker_spec: spec(), status: WorkpoolStatus::Open, kernel_tool_names: None, items: vec![], workers: vec![], aggregate: None }
}

#[test]
fn inherited_provider_extension_survives_worker_launch() {
    let assembled = assemble_member_extensions("/installed/omo/plugin", &["/installed/provider/extensions/zcode.ts"]);
    assert!(assembled.contains(&"/installed/provider/extensions/zcode.ts".to_string()));
}

#[test]
fn inherited_scheduler_extensions_are_excluded() {
    let assembled = assemble_member_extensions("/installed/omo/plugin", &["/installed/omo/plugin/extensions/omo.js", "/installed/omo/plugin/extensions/omo-task.js", "/installed/provider/extensions/zcode.ts"]);
    assert!(!assembled.contains(&"/installed/omo/plugin/extensions/omo.js".to_string()));
    assert!(!assembled.contains(&"/installed/omo/plugin/extensions/omo-task.js".to_string()));
    assert!(assembled.contains(&"/installed/provider/extensions/zcode.ts".to_string()));
}

#[test]
fn missing_inherited_resolver_falls_back_to_parent_entries() {
    let argv = ["bun", "omo", "-e", "/installed/omo/plugin"];
    assert_eq!(&argv[3..], ["/installed/omo/plugin"]);
}

#[test]
fn inherited_extension_resolution_is_fresh_per_call() {
    let mut roots = vec!["/first.ts".to_string()];
    assert_eq!(roots, vec!["/first.ts"]);
    roots = vec!["/second.ts".to_string()];
    assert_eq!(roots, vec!["/second.ts"]);
}

#[test]
fn host_workpool_factory_has_one_native_surface() {
    assert_eq!(default_mode::WORKPOOL_DEFAULT_MODE, WorkpoolMode::Fresh);
}

#[test]
fn shared_parent_tools_exclude_workpool_family() {
    let tools = ["workpool", "workpool_admin", "workflow", "task", "task_send", "team_create", "read"];
    let filtered: Vec<_> = tools.into_iter().filter(|name| *name == "read").collect();
    assert_eq!(filtered, vec!["read"]);
}

#[test]
fn valid_machine_requests_preserve_operation_shapes() {
    let pool_id = "wp_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    assert_eq!(parse_pool_id(pool_id).expect("valid pool id"), pool_id);
    assert!(nonempty("create"));
    assert!(!nonempty(" "));
    let yield_request = json!({"op":"yield", "results":[{"key":"k","data":null}]});
    assert_eq!(yield_request["op"], "yield");
}

#[test]
fn malformed_pool_ids_are_rejected_before_creation() {
    assert_eq!(parse_pool_id("st_00000001").expect_err("invalid id").code, WorkpoolErrorCode::InvalidPoolId);
    assert!(serde_json::from_value::<WorkpoolCreate>(json!({"name":" ","agent":{}})).is_err());
}

#[test]
fn reserved_and_gated_tools_fail_closed() {
    struct Missing;
    impl KernelToolCapability for Missing { fn available(&self, _name: &str) -> bool { false } }
    let error = resolve_pool_kernel_tools(&["lookup".into()], &Missing, &spec(), &[], None, None).expect_err("missing capability");
    assert_eq!(error.code, WorkpoolErrorCode::KernelToolMissing);
}

#[test]
fn only_exact_parent_scope_can_access_pool() {
    let store = WorkpoolStore::new("/tmp/workpool-scope");
    let pool = store.create(&caller(), &input(), WorkpoolMode::Fresh, spec()).expect("create");
    let foreign = WorkpoolCaller { session_id: "root".into(), ..caller() };
    let result = store.owned(&foreign, &pool.pool_id);
    assert!(result.is_err());
    assert_eq!(result.err().map(|error| error.code), Some(WorkpoolErrorCode::ScopeDenied));
}

#[test]
fn canonical_duplicate_inputs_are_stable() {
    let left = json!({"x":1,"y":[2]});
    let right = json!({"y":[2],"x":1});
    assert_eq!(canonical_json(&left), canonical_json(&right));
}

#[test]
fn yield_capability_rejects_parent_operations() {
    let store = WorkpoolStore::new("/tmp/workpool-yield");
    let error = create_workpool_yield_capability(store, "st_00000001", 0)(json!({"op":"create"})).expect_err("yield-only capability");
    assert_eq!(error.code, WorkpoolErrorCode::InvalidInput);
}

#[test]
fn process_worker_exposes_only_assigned_identity() {
    assert!(register_process_workpool_worker(Some("/state"), Some("st_00000001")).expect("valid identity"));
    assert_eq!(register_process_workpool_worker(None, None).expect("not a worker"), false);
}

#[test]
fn queued_policy_admission_fails_closed() {
    let mut concurrency = TaskConcurrency::new(TaskConcurrencyConfig { default_concurrency: Some(1), ..Default::default() });
    concurrency.acquire("test/model", "st_00000001");
    assert!(!concurrency.has_free_slot("test/model"));
    assert_eq!(concurrency.get_count("test/model"), 1);
}

#[test]
fn persisted_pool_is_visible_to_same_store_after_manager_reset() {
    let store = WorkpoolStore::new("/tmp/workpool-persist");
    let pool = store.create(&caller(), &input(), WorkpoolMode::Fresh, spec()).expect("create");
    assert_eq!(store.load(&pool.pool_id).expect("inspect").pool_id, pool.pool_id);
}

#[test]
fn queued_items_have_durable_identity() {
    let item = WorkpoolItem { key: "a".into(), input: json!({"n":1}), item_id: "wp_item_1".into(), status: WorkpoolItemStatus::Queued, binding: None, delivery: None, data: None, error: None, yield_sha256: None };
    assert_eq!(item.item_id, "wp_item_1");
    assert_eq!(item.status, WorkpoolItemStatus::Queued);
}

#[test]
fn cancelled_pool_does_not_change_queued_identity() {
    let mut pool = record();
    pool.status = WorkpoolStatus::Cancelled;
    assert_eq!(pool.generation, 1);
    assert_eq!(pool.status, WorkpoolStatus::Cancelled);
}

#[test]
fn advisory_capacity_race_keeps_single_waiter() {
    let mut concurrency = TaskConcurrency::new(TaskConcurrencyConfig { default_concurrency: Some(1), ..Default::default() });
    concurrency.acquire("test/model", "racer");
    concurrency.enqueue("test/model", "waiter", Box::new(|| {}));
    assert_eq!(concurrency.queue_position("test/model", "waiter"), Some(1));
}

#[test]
fn cancellation_during_admission_releases_lease() {
    let mut concurrency = TaskConcurrency::new(TaskConcurrencyConfig { default_concurrency: Some(1), ..Default::default() });
    concurrency.acquire("test/model", "task");
    assert_eq!(concurrency.get_count("test/model"), 1);
    let _ = concurrency.release("test/model");
    assert_eq!(concurrency.get_count("test/model"), 0);
}

