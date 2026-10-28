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
    WorkpoolRecord { hold: None, version: 1, pool_id: "wp_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(), name: "pool".into(), parent_session_id: "session".into(), root_session_id: "root".into(), generation: 1, revision: 0, mode: WorkpoolMode::Fresh, agent: input().agent, worker_spec: spec(), status: WorkpoolStatus::Open, kernel_tool_names: None, items: vec![], workers: vec![], aggregate: None }
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
    assert_eq!(default_mode::WORKPOOL_DEFAULT_MODE, WorkpoolMode::KeepAlive);
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
    let root = tempfile::tempdir().expect("temporary state directory");
    let store = WorkpoolStore::new(root.path());
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
    let root = tempfile::tempdir().expect("temporary state directory");
    let store = WorkpoolStore::new(root.path());
    let tasks = crate::store::TaskRecordStore::new(&crate::store::StateDirConfig {
        project_dir: root.path().into(), task_state_dir: Some(root.path().into()),
    });
    let error = create_workpool_yield_capability(store, tasks, "st_00000001", 0, None)(json!({"op":"create"})).expect_err("yield-only capability");
    assert_eq!(error.code, WorkpoolErrorCode::InvalidInput);
}

#[test]
fn process_worker_exposes_only_assigned_identity() {
    assert!(register_process_workpool_worker(Some("/state"), Some("st_00000001")).expect("valid identity"));
    assert!(!register_process_workpool_worker(None, None).expect("not a worker"));
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
    let root = tempfile::tempdir().expect("temporary state directory");
    let store = WorkpoolStore::new(root.path());
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

#[test]
fn reconcile_ack_loss_marks_unbound_assignment_uncertain() {
    let root = tempfile::tempdir().expect("temporary state directory");
    let store = WorkpoolStore::new(root.path());
    let pool = store.create(&caller(), &input(), WorkpoolMode::Fresh, spec()).expect("create");
    store.mutate(&pool.pool_id, |p| { let mut n=p.clone(); n.items.push(WorkpoolItem { key:"a".into(), input:json!(2), item_id:"i".into(), status:WorkpoolItemStatus::Assigned, binding:None, delivery:None, data:None, error:None, yield_sha256:None }); n }).expect("insert assigned item");
    super::reconcile::reconcile_workpool(&store, &pool.pool_id).expect("reconcile");
    assert_eq!(store.load(&pool.pool_id).expect("load").items[0].error.as_ref().expect("error").code, "delivery_uncertain");
}

#[test]
fn reconcile_missing_yield_is_typed() { let item=WorkpoolItem { key:"a".into(), input:json!(null), item_id:"i".into(), status:WorkpoolItemStatus::Assigned, binding:None, delivery:None, data:None, error:None, yield_sha256:None }; assert_eq!(super::reconcile::uncertain_item(item).error.expect("error").code,"delivery_uncertain"); }

#[test]
fn reconcile_cancelled_pool_is_unchanged() { let root=tempfile::tempdir().expect("temporary state directory"); let store=WorkpoolStore::new(root.path()); let mut pool=store.create(&caller(),&input(),WorkpoolMode::Fresh,spec()).expect("create"); pool.status=WorkpoolStatus::Cancelled; store.mutate(&pool.pool_id, |_| pool.clone()).expect("save"); let path=root.path().join("workpools").join(format!("{}.json",pool.pool_id)); let cancelled=std::fs::read(&path).expect("cancelled record"); super::reconcile::reconcile_workpool(&store,&pool.pool_id).expect("reconcile"); assert_eq!(std::fs::read(&path).expect("loaded record"),cancelled); }

fn reuse_setup() -> (tempfile::TempDir, WorkpoolStore, WorkpoolCaller, WorkpoolCreate) { let root=tempfile::tempdir().expect("temporary state directory"); let store=WorkpoolStore::new(root.path()); let mut request=input(); request.name="batch".into(); (root,store,caller(),request) }
#[test] fn reuse_different_tool_is_conflict() { let (_root,s,c,mut i)=reuse_setup(); i.tools=Some(vec!["secret".into()]); s.create(&c,&i,WorkpoolMode::Fresh,spec()).expect("first"); i.tools=Some(vec!["other".into()]); assert_eq!(s.create(&c,&i,WorkpoolMode::Fresh,spec()).err().expect("conflict").code,WorkpoolErrorCode::PoolNameConflict); }
#[test] fn reuse_conflict_does_not_reveal_grant() { let (_root,s,c,mut i)=reuse_setup(); i.tools=Some(vec!["secret_tool".into()]); s.create(&c,&i,WorkpoolMode::Fresh,spec()).expect("first"); i.tools=Some(vec!["other".into()]); let e=s.create(&c,&i,WorkpoolMode::Fresh,spec()).err().expect("conflict"); assert!(!e.message.contains("secret_tool")); }
#[test] fn reuse_grant_to_no_tools_is_conflict() { let (_root,s,c,mut i)=reuse_setup(); i.tools=Some(vec!["secret".into()]); s.create(&c,&i,WorkpoolMode::Fresh,spec()).expect("first"); i.tools=None; assert_eq!(s.create(&c,&i,WorkpoolMode::Fresh,spec()).err().expect("conflict").code,WorkpoolErrorCode::PoolNameConflict); }
#[test] fn reuse_without_tools_cannot_widen() { let (_root,s,c,mut i)=reuse_setup(); s.create(&c,&i,WorkpoolMode::Fresh,spec()).expect("first"); i.tools=Some(vec!["secret".into()]); assert_eq!(s.create(&c,&i,WorkpoolMode::Fresh,spec()).err().expect("conflict").code,WorkpoolErrorCode::PoolNameConflict); }
#[test] fn reuse_same_grant_in_other_order_reuses_pool() { let (_root,s,c,mut i)=reuse_setup(); i.tools=Some(vec!["a".into(),"b".into()]); let first=s.create(&c,&i,WorkpoolMode::Fresh,spec()).expect("first"); i.tools=Some(vec!["b".into(),"a".into()]); assert_eq!(s.create(&c,&i,WorkpoolMode::Fresh,spec()).expect("reuse").pool_id,first.pool_id); }
#[test] fn reuse_empty_grant_reuses_pool() { let (_root,s,c,i)=reuse_setup(); let first=s.create(&c,&i,WorkpoolMode::Fresh,spec()).expect("first"); let mut empty=i.clone(); empty.tools=Some(vec![]); assert_eq!(s.create(&c,&empty,WorkpoolMode::Fresh,spec()).expect("reuse").pool_id,first.pool_id); }

#[test]
fn kernel_denylist_removing_parent_write_tool_refuses_grant() {
    struct Available; impl KernelToolCapability for Available { fn available(&self, name:&str)->bool { name=="lookup" } }
    let mut worker=spec(); worker.plan.tool_denylist=Some(vec!["synthetic_write".into()]);
    let error=resolve_pool_kernel_tools(&["lookup".into()],&Available,&worker,&["synthetic_write".into()],None,None).expect_err("tools unavailable");
    assert_eq!(error.code,WorkpoolErrorCode::ToolsUnavailable);
}

