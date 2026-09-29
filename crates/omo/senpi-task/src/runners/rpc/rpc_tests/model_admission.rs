//! `runners/rpc/model-admission.test.ts` and `runners/rpc/model-admission-cache.test.ts`.

use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use super::fake_child::spawn_fake_child;
use crate::runners::rpc::model_admission::{
    MODEL_CATALOG_CACHE_TTL_MS, ModelCatalogProbeResult, RpcModelAdmissionOptions,
    create_rpc_model_admission, parse_model_catalog, probe_model_catalog,
};
use crate::runners::rpc::process::{RpcChildProcess, RpcSpawnDescriptor};
use crate::runners::rpc_process::{RpcProcessRunner, RpcProcessRunnerOptions};
use crate::runners::types::{RpcRunnerSpec, TerminateOptions};
use crate::runners::{RunnerFailure, RunnerFailureKind};

fn shell_descriptor(script: &str) -> RpcSpawnDescriptor {
    RpcSpawnDescriptor {
        command: "/bin/sh".to_string(),
        args: vec!["-c".to_string(), script.to_string()],
        cwd: std::env::temp_dir().to_string_lossy().into_owned(),
        env: std::env::vars().collect(),
    }
}

fn spec(model: &str, state_dir: &str) -> RpcRunnerSpec {
    RpcRunnerSpec {
        task_id: "st_model_admission".to_string(),
        cwd: std::env::current_dir()
            .expect("cwd")
            .to_string_lossy()
            .into_owned(),
        state_dir: state_dir.to_string(),
        prompt: "hello".to_string(),
        model: Some(model.to_string()),
        ..RpcRunnerSpec::default()
    }
}

#[test]
fn given_a_catalog_probe_when_spawned_then_it_is_hidden_and_owns_its_posix_process_tree() {
    let result = probe_model_catalog(
        &shell_descriptor("ps -o pgid= -p $$; ps -o pid= -p $$"),
        None,
    );
    assert_eq!(result.code, Some(0));
    assert!(!result.timed_out);
    let mut ids = result.stdout.split_whitespace();
    let pgid = ids.next().expect("pgid");
    let pid = ids.next().expect("pid");
    assert_eq!(pgid, pid, "probe must lead its own process group");
}

#[test]
fn given_a_timed_out_catalog_probe_when_tree_cleanup_is_pending_then_the_timeout_result_waits_for_cleanup()
 {
    let started = Instant::now();
    let result = probe_model_catalog(&shell_descriptor("trap '' TERM; sleep 30"), Some(0));
    assert!(result.timed_out);
    assert_eq!(result.code, None);
    assert!(
        started.elapsed().as_secs() < 20,
        "cleanup must escalate, not wait for the sleep"
    );
}

#[test]
fn given_a_senpi_model_table_when_parsed_then_provider_and_model_columns_form_exact_identities() {
    let output = [
        "fixture  visible       128K  8K  yes  no",
        "fixture  visible-fast  128K  8K  yes  no",
        "other    visible       128K  8K  yes  no",
    ]
    .join("\n");
    let catalog = parse_model_catalog(&output);
    assert!(catalog.contains("fixture/visible"));
    assert!(!catalog.contains("fixture/vis"));
    assert!(catalog.contains("other/visible"));
}

#[test]
fn given_a_compact_provider_model_identity_line_when_parsed_then_the_exact_identity_is_visible() {
    let output = [
        "provider                    model                                                     context  max-out  thinking  images",
        "omo-mock/mock-1",
    ]
    .join("\n");
    assert!(parse_model_catalog(&output).contains("omo-mock/mock-1"));
}

fn tracking_runner(
    admission: crate::runners::rpc::model_admission::RpcModelAdmission,
    order: &Arc<Mutex<Vec<&'static str>>>,
    spawned: &Arc<Mutex<Vec<Arc<RpcChildProcess>>>>,
) -> RpcProcessRunner {
    let order = Arc::clone(order);
    let spawned = Arc::clone(spawned);
    RpcProcessRunner::new(RpcProcessRunnerOptions {
        model_admission: Some(admission),
        spawn_child: Some(Arc::new(move |_descriptor: &RpcSpawnDescriptor| {
            order.lock().expect("order").push("spawn");
            let child = spawn_fake_child(&[]);
            spawned.lock().expect("spawned").push(Arc::clone(&child));
            child
        })),
        ..RpcProcessRunnerOptions::default()
    })
}

fn terminate_all(spawned: &Mutex<Vec<Arc<RpcChildProcess>>>) {
    for child in spawned.lock().expect("spawned").drain(..) {
        let _ = crate::runners::rpc::terminate::terminate_rpc_child(
            &child,
            TerminateOptions {
                sigkill_delay_ms: Some(200),
            },
        );
    }
}

#[test]
fn given_a_model_absent_from_the_child_profile_when_started_then_admission_rejects_before_spawn() {
    let state = tempfile::tempdir().expect("state dir");
    let admission_calls = Arc::new(AtomicUsize::new(0));
    let calls = Arc::clone(&admission_calls);
    let order = Arc::default();
    let spawned = Arc::default();
    let runner = tracking_runner(
        Arc::new(move |_spec: &RpcRunnerSpec| {
            calls.fetch_add(1, Ordering::SeqCst);
            Err(RunnerFailure::new(
                RunnerFailureKind::ModelUnavailable,
                "model fixture/missing is not visible to the process child",
            ))
        }),
        &order,
        &spawned,
    );
    let started = runner.start(&spec("fixture/missing", &state.path().to_string_lossy()));
    let failure = started.err().expect("admission must reject");
    assert_eq!(failure.kind, RunnerFailureKind::ModelUnavailable);
    assert_eq!(admission_calls.load(Ordering::SeqCst), 1);
    assert!(order.lock().expect("order").is_empty());
}

#[test]
fn given_a_model_visible_to_the_child_profile_when_started_then_admission_completes_before_spawn() {
    let state = tempfile::tempdir().expect("state dir");
    let order: Arc<Mutex<Vec<&'static str>>> = Arc::default();
    let spawned = Arc::default();
    let admit_order = Arc::clone(&order);
    let runner = tracking_runner(
        Arc::new(move |_spec: &RpcRunnerSpec| {
            admit_order.lock().expect("order").push("admit");
            Ok(())
        }),
        &order,
        &spawned,
    );
    let handle = runner
        .start(&spec("fixture/visible", &state.path().to_string_lossy()))
        .expect("start");
    assert_eq!(*order.lock().expect("order"), vec!["admit", "spawn"]);
    handle.dispose_handle();
    terminate_all(&spawned);
}

fn cache_descriptor() -> RpcSpawnDescriptor {
    RpcSpawnDescriptor {
        command: "senpi".to_string(),
        args: vec!["--list-models".to_string()],
        cwd: "/tmp".to_string(),
        env: [("HOME".to_string(), "/tmp".to_string())].into(),
    }
}

fn cache_spec(model: &str) -> RpcRunnerSpec {
    RpcRunnerSpec {
        task_id: "st_cache".to_string(),
        cwd: "/tmp".to_string(),
        state_dir: "/tmp/state".to_string(),
        prompt: "hello".to_string(),
        model: Some(model.to_string()),
        ..RpcRunnerSpec::default()
    }
}

fn catalog_output(models: &[String]) -> String {
    std::iter::once("provider  model".to_string())
        .chain(models.iter().map(|entry| entry.replacen('/', "  ", 1)))
        .collect::<Vec<_>>()
        .join("\n")
}

fn counting_admission(
    visible: &Arc<Mutex<Vec<String>>>,
    clock: &Arc<AtomicI64>,
    probes: &Arc<AtomicUsize>,
) -> crate::runners::rpc::model_admission::RpcModelAdmission {
    let visible = Arc::clone(visible);
    let clock = Arc::clone(clock);
    let probes = Arc::clone(probes);
    create_rpc_model_admission(RpcModelAdmissionOptions {
        build_spawn: Some(Arc::new(|_spec: &RpcRunnerSpec| cache_descriptor())),
        probe: Some(Arc::new(move |_descriptor: &RpcSpawnDescriptor| {
            probes.fetch_add(1, Ordering::SeqCst);
            ModelCatalogProbeResult {
                code: Some(0),
                stdout: catalog_output(&visible.lock().expect("visible")),
                stderr: String::new(),
                timed_out: false,
            }
        })),
        now: Some(Arc::new(move || clock.load(Ordering::SeqCst))),
    })
}

#[test]
fn given_a_cached_successful_catalog_whose_ttl_has_expired_when_a_later_admission_runs_then_the_catalog_is_re_probed_instead_of_served_stale()
 {
    let visible = Arc::new(Mutex::new(vec!["prov/alpha".to_string()]));
    let clock = Arc::new(AtomicI64::new(1_000));
    let probes = Arc::new(AtomicUsize::new(0));
    let admit = counting_admission(&visible, &clock, &probes);
    admit(&cache_spec("prov/alpha")).expect("alpha admitted");
    assert_eq!(probes.load(Ordering::SeqCst), 1);
    visible
        .lock()
        .expect("visible")
        .push("prov/beta".to_string());
    clock.fetch_add(MODEL_CATALOG_CACHE_TTL_MS + 1, Ordering::SeqCst);
    admit(&cache_spec("prov/beta")).expect("beta admitted");
    assert_eq!(probes.load(Ordering::SeqCst), 2);
}

#[test]
fn given_a_cached_successful_catalog_inside_its_ttl_when_a_later_admission_runs_then_the_cached_catalog_is_reused()
 {
    let visible = Arc::new(Mutex::new(vec!["prov/alpha".to_string()]));
    let clock = Arc::new(AtomicI64::new(1_000));
    let probes = Arc::new(AtomicUsize::new(0));
    let admit = counting_admission(&visible, &clock, &probes);
    admit(&cache_spec("prov/alpha")).expect("first");
    clock.fetch_add(MODEL_CATALOG_CACHE_TTL_MS / 2, Ordering::SeqCst);
    admit(&cache_spec("prov/alpha")).expect("second");
    assert_eq!(probes.load(Ordering::SeqCst), 1);
}
