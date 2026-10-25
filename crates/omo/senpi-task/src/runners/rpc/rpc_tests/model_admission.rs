//! `runners/rpc/model-admission.test.ts` and `runners/rpc/model-admission-cache.test.ts`.

use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use super::fake_child::spawn_fake_child;
use crate::runners::rpc::model_admission::{
    MODEL_CATALOG_CACHE_TTL_MS, ModelCatalogProbeResult, RpcModelAdmissionOptions,
    create_rpc_model_admission, parse_model_catalog,
};
use crate::runners::rpc::process::{RpcChildProcess, RpcSpawnDescriptor};
use crate::runners::rpc_process::{RpcProcessRunner, RpcProcessRunnerOptions};
use crate::runners::types::{RpcRunnerSpec, TerminateOptions};
use crate::runners::{RunnerFailure, RunnerFailureKind};

#[cfg(unix)]
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
#[cfg(unix)]
fn given_a_catalog_probe_when_spawned_then_it_is_hidden_and_owns_its_posix_process_tree() {
    let result = crate::runners::rpc::model_admission::probe_model_catalog(
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
#[cfg(unix)]
fn given_a_timed_out_catalog_probe_when_tree_cleanup_is_pending_then_the_timeout_result_waits_for_cleanup()
 {
    let started = std::time::Instant::now();
    let result = crate::runners::rpc::model_admission::probe_model_catalog(
        &shell_descriptor("trap '' TERM; sleep 30"),
        Some(0),
    );
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
        ..RpcModelAdmissionOptions::default()
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

#[test]
fn inconclusive_probes_retry_admit_and_never_cache() {
    let probes = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&probes);
    let clock = Arc::new(AtomicI64::new(1_000));
    let time = Arc::clone(&clock);
    let read_time = Arc::clone(&clock);
    let verdicts = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&verdicts);
    let admit = create_rpc_model_admission(RpcModelAdmissionOptions {
        build_spawn: Some(Arc::new(|_| cache_descriptor())),
        probe: Some(Arc::new(move |_| {
            count.fetch_add(1, Ordering::SeqCst);
            time.fetch_add(20_000, Ordering::SeqCst);
            ModelCatalogProbeResult {
                timed_out: true,
                ..Default::default()
            }
        })),
        now: Some(Arc::new(move || read_time.load(Ordering::SeqCst))),
        on_inconclusive: Some(Arc::new(move |v| {
            recorded.lock().expect("verdicts").push(v)
        })),
        ..Default::default()
    });
    admit(&cache_spec("prov/alpha")).expect("admit");
    admit(&cache_spec("prov/alpha")).expect("admit again");
    assert_eq!(probes.load(Ordering::SeqCst), 4);
    let verdicts = verdicts.lock().expect("verdicts");
    assert_eq!(verdicts.len(), 2);
    assert_eq!(verdicts[0].verdict, "inconclusive");
    assert_eq!(verdicts[0].elapsed_ms, 40_000);
}

#[test]
fn stale_catalog_only_admits_within_half_hour_and_never_proves_absence() {
    for (advance, requested, expected) in [
        (
            MODEL_CATALOG_CACHE_TTL_MS + 600_000,
            "prov/alpha",
            "stale_catalog",
        ),
        (MODEL_CATALOG_CACHE_TTL_MS + 1, "prov/beta", "inconclusive"),
        (
            super::super::model_admission::MODEL_CATALOG_MAX_STALE_MS + 1,
            "prov/alpha",
            "inconclusive",
        ),
    ] {
        let probes = Arc::new(AtomicUsize::new(0));
        let count = Arc::clone(&probes);
        let clock = Arc::new(AtomicI64::new(1_000));
        let time = Arc::clone(&clock);
        let verdicts = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&verdicts);
        let admit = create_rpc_model_admission(RpcModelAdmissionOptions {
            build_spawn: Some(Arc::new(|_| cache_descriptor())),
            probe: Some(Arc::new(move |_| {
                if count.fetch_add(1, Ordering::SeqCst) == 0 {
                    ModelCatalogProbeResult {
                        code: Some(0),
                        stdout: "prov alpha".into(),
                        ..Default::default()
                    }
                } else {
                    ModelCatalogProbeResult {
                        timed_out: true,
                        ..Default::default()
                    }
                }
            })),
            now: Some(Arc::new(move || time.load(Ordering::SeqCst))),
            on_inconclusive: Some(Arc::new(move |v| {
                recorded.lock().expect("verdicts").push(v)
            })),
            ..Default::default()
        });
        admit(&cache_spec("prov/alpha")).expect("initial");
        clock.fetch_add(advance, Ordering::SeqCst);
        admit(&cache_spec(requested)).expect("unjudged");
        assert_eq!(verdicts.lock().expect("verdicts")[0].verdict, expected);
        assert_eq!(probes.load(Ordering::SeqCst), 3);
    }
}

#[test]
fn confirmation_timeout_admits_but_confirmed_absence_and_nonzero_exit_reject() {
    for mode in 0..3 {
        let count = Arc::new(AtomicUsize::new(0));
        let probes = Arc::clone(&count);
        let admit = create_rpc_model_admission(RpcModelAdmissionOptions {
            build_spawn: Some(Arc::new(|_| cache_descriptor())),
            probe: Some(Arc::new(move |_| {
                let attempt = probes.fetch_add(1, Ordering::SeqCst);
                if mode == 0 && attempt > 0 {
                    ModelCatalogProbeResult {
                        timed_out: true,
                        ..Default::default()
                    }
                } else {
                    ModelCatalogProbeResult {
                        code: Some(if mode == 2 { 1 } else { 0 }),
                        stdout: "prov alpha".into(),
                        ..Default::default()
                    }
                }
            })),
            ..Default::default()
        });
        assert_eq!(admit(&cache_spec("prov/beta")).is_ok(), mode == 0);
        assert_eq!(
            count.load(Ordering::SeqCst),
            if mode == 0 {
                3
            } else if mode == 1 {
                2
            } else {
                1
            }
        );
    }
}

#[test]
fn decorated_models_admit_and_exclusions_precede_probes() {
    let admit = create_rpc_model_admission(RpcModelAdmissionOptions {
        build_spawn: Some(Arc::new(|_| cache_descriptor())),
        probe: Some(Arc::new(|_| ModelCatalogProbeResult {
            code: Some(0),
            stdout: "prov alpha".into(),
            ..Default::default()
        })),
        exclude_models: vec!["BETA*".into(), "vendor/hidden".into()],
        ..Default::default()
    });
    admit(&cache_spec("prov/alpha:high:priority")).expect("decorated");
    assert!(admit(&cache_spec("prov/beta:high")).is_err());
    assert!(admit(&cache_spec("gateway/vendor/hidden")).is_err());
}
