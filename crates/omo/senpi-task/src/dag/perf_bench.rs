//! Perf bench (bench branch only, never part of the suite). Drives the real DagManager +
//! DagScheduler + DagWaitSurface + TaskManager with a runner that completes every node instantly,
//! so the numbers are the engine's own CPU, allocation and memory cost with no model in the loop.
//! Also times the per-message ultrawork / mass-ulw input hooks. Run explicitly:
//! `<test-bin> perf_bench --ignored --nocapture --test-threads=1`; each scenario prints one
//! `BENCH {json}` line with medians over the measured iterations.
#![allow(clippy::unwrap_used, unsafe_code)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering::Relaxed};
use std::time::Instant;

use crate::dag::graph::{DagDefinition, DagNodeInput};
use crate::dag::handle::{DagWaitSurfaceOptions, create_dag_wait_surface};
use crate::dag::manager::{DagHistoryParams, DagManager, DagManagerOptions, DagStartParams, create_dag_manager};
use crate::dag::scheduler::{DagSchedulerOptions, create_dag_scheduler};
use crate::dag::store::{DagFileStore, DagStoreConfig, DagStoreOptions, create_dag_file_store};
use crate::dag::types::{DagNodeTarget, DagRunId, DagRunStatus};
use crate::manager::TaskManager;
use crate::manager::manager_tests::fakes::{FakeHandle, FakeRunner, HarnessOptions, Project, config, make_manager};

struct Counting;
static ALLOCS: AtomicU64 = AtomicU64::new(0);
static BYTES: AtomicU64 = AtomicU64::new(0);
static LIVE: AtomicI64 = AtomicI64::new(0);
static PEAK: AtomicI64 = AtomicI64::new(0);

fn note_alloc(size: usize) {
    ALLOCS.fetch_add(1, Relaxed);
    BYTES.fetch_add(size as u64, Relaxed);
    let live = LIVE.fetch_add(size as i64, Relaxed) + size as i64;
    PEAK.fetch_max(live, Relaxed);
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        note_alloc(layout.size());
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        note_alloc(layout.size());
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size() as i64, Relaxed);
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        LIVE.fetch_sub(layout.size() as i64, Relaxed);
        note_alloc(new_size);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

fn cpu_us() -> u64 {
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) };
    let micros = |tv: libc::timeval| tv.tv_sec as u64 * 1_000_000 + tv.tv_usec as u64;
    micros(usage.ru_utime) + micros(usage.ru_stime)
}

#[derive(Clone, Copy)]
struct Sample {
    wall_us: u64,
    cpu_us: u64,
    allocs: u64,
    bytes: u64,
    peak: u64,
}

fn measure<T>(f: impl FnOnce() -> T) -> (Sample, T) {
    let (allocs0, bytes0, live0) = (ALLOCS.load(Relaxed), BYTES.load(Relaxed), LIVE.load(Relaxed));
    PEAK.store(live0, Relaxed);
    let cpu0 = cpu_us();
    let start = Instant::now();
    let out = f();
    let wall_us = start.elapsed().as_micros() as u64;
    let sample = Sample {
        wall_us,
        cpu_us: cpu_us() - cpu0,
        allocs: ALLOCS.load(Relaxed) - allocs0,
        bytes: BYTES.load(Relaxed) - bytes0,
        peak: (PEAK.load(Relaxed) - live0).max(0) as u64,
    };
    (sample, out)
}

fn median(mut values: Vec<u64>) -> u64 {
    values.sort_unstable();
    values[values.len() / 2]
}

fn report(scenario: &str, per_iter_ops: u64, samples: &[Sample]) {
    let pick = |f: fn(&Sample) -> u64| median(samples.iter().map(f).collect());
    println!(
        "BENCH {{\"scenario\":\"{scenario}\",\"ops\":{per_iter_ops},\"n\":{},\"wall_us\":{},\"cpu_us\":{},\"allocs\":{},\"bytes\":{},\"peak_bytes\":{}}}",
        samples.len(),
        pick(|s| s.wall_us),
        pick(|s| s.cpu_us),
        pick(|s| s.allocs),
        pick(|s| s.bytes),
        pick(|s| s.peak),
    );
}

const PARENT: &str = "session-bench-parent";
const ROOT: &str = "session-bench-root";

fn node(id: String, depends_on: Vec<String>) -> DagNodeInput {
    DagNodeInput {
        prompt: format!("do {id}"),
        id,
        target: DagNodeTarget::Category("quick".to_string()),
        label: None,
        depends_on: if depends_on.is_empty() { None } else { Some(depends_on) },
        task_summary: None,
        description: None,
        load_skills: None,
    }
}

fn wide(key: &str) -> DagDefinition {
    let roots: Vec<String> = (0..63).map(|i| format!("n{i}")).collect();
    let mut nodes: Vec<DagNodeInput> = roots.iter().map(|id| node(id.clone(), Vec::new())).collect();
    nodes.push(node("join".to_string(), roots));
    DagDefinition { key: key.to_string(), name: key.to_string(), nodes }
}

fn layered(key: &str) -> DagDefinition {
    let mut nodes = Vec::new();
    for layer in 0..8 {
        for i in 0..8 {
            let deps = if layer == 0 { Vec::new() } else { (0..8).map(|j| format!("l{}n{j}", layer - 1)).collect() };
            nodes.push(node(format!("l{layer}n{i}"), deps));
        }
    }
    DagDefinition { key: key.to_string(), name: key.to_string(), nodes }
}

fn chain(key: &str) -> DagDefinition {
    let nodes = (0..64).map(|i| node(format!("c{i}"), if i == 0 { Vec::new() } else { vec![format!("c{}", i - 1)] })).collect();
    DagDefinition { key: key.to_string(), name: key.to_string(), nodes }
}

struct Fixture {
    _project: Project,
    store: Arc<DagFileStore>,
    manager: DagManager,
    tasks: Arc<TaskManager>,
}

fn fixture() -> Fixture {
    let runner = FakeRunner::new();
    *runner.hook.lock().unwrap() = Some(Arc::new(|spec, _| {
        let handle = FakeHandle::new(&spec.task_id, None);
        handle.complete(&format!("output:{}", spec.task_id));
        Some(Ok(handle as Arc<dyn crate::manager::ManagedChildHandle>))
    }));
    let harness = make_manager(HarnessOptions {
        config: Some(config(64, 1)),
        in_process: Some(Arc::clone(&runner)),
        process: Some(Arc::clone(&runner)),
        planner: Some(Arc::new(|spec| {
            let target = spec.category.as_deref().or(spec.subagent_type.as_deref()).unwrap_or("default");
            Ok(crate::manager::types::ResolvedChildPlan {
                model: format!("scripted/{target}"),
                category: spec.category.clone(),
                agent_type: spec.subagent_type.clone(),
                ..Default::default()
            })
        })),
        ..Default::default()
    });
    let store = Arc::new(
        create_dag_file_store(
            &DagStoreConfig::new(harness.project.dir.path()),
            DagStoreOptions { fsync: Some(false), ..Default::default() },
        )
        .unwrap(),
    );
    let manager = create_dag_manager(DagManagerOptions { store: Arc::clone(&store), new_run_id: None, now: None, materialize_skills: None, settings: None });
    Fixture { _project: harness.project, store, manager, tasks: Arc::new(harness.manager) }
}

fn run_once(fixture: &Fixture, definition: DagDefinition) -> DagRunId {
    let started = fixture
        .manager
        .start(DagStartParams { definition, parent_session_id: PARENT.into(), root_session_id: ROOT.into() })
        .unwrap();
    let run_id = started.snapshot.run_id.clone();
    let scheduler = create_dag_scheduler(DagSchedulerOptions {
        store: Arc::clone(&fixture.store),
        task_manager: Arc::clone(&fixture.tasks),
        initial_record: fixture.manager.record(&run_id, PARENT).unwrap(),
        execution_mode: None,
        ancestry: None,
        node_spawn_policy: None,
        pre_attached_tasks: None,
        subscriber_ring: None,
        now: None,
        schedule_retry: None,
        resume_credential: None,
    })
    .unwrap();
    scheduler.run().unwrap();
    let wait = create_dag_wait_surface(DagWaitSurfaceOptions {
        store: Arc::clone(&fixture.store),
        subscribe: Arc::new(|_, _| Box::new(|| {})),
        cancel: None,
        read_output: None,
    });
    let result = wait.wait(&run_id, PARENT).unwrap();
    assert_eq!(result.status, DagRunStatus::Completed);
    run_id
}

#[test]
#[ignore = "perf bench; run explicitly"]
fn perf_bench_dag() {
    const WARMUP: usize = 2;
    const ITERS: usize = 15;
    type Scenario = (&'static str, fn(&str) -> DagDefinition);
    let scenarios: [Scenario; 3] = [("dag_wide64", wide), ("dag_layered8x8", layered), ("dag_chain64", chain)];
    for (name, build) in scenarios {
        let mut samples = Vec::new();
        for iteration in 0..WARMUP + ITERS {
            let fixture = fixture();
            let definition = build(&format!("{name}-{iteration}"));
            let (sample, _) = measure(|| run_once(&fixture, definition));
            if iteration >= WARMUP {
                samples.push(sample);
            }
        }
        report(name, 64, &samples);
    }

    const POLLS: u64 = 200;
    let fixture = fixture();
    let run_id = run_once(&fixture, wide("poll"));
    let mut samples = Vec::new();
    for iteration in 0..WARMUP + 7 {
        let (sample, _) = measure(|| {
            for _ in 0..POLLS {
                black_box(fixture.manager.snapshot(&run_id, PARENT).unwrap());
                black_box(fixture.manager.list(PARENT, None).unwrap());
                black_box(
                    fixture
                        .manager
                        .history(DagHistoryParams {
                            run_id: run_id.clone(),
                            parent_session_id: PARENT.into(),
                            since_seq: None,
                            limit: Some(256),
                            lane: None,
                            types: None,
                            through_seq: None,
                        })
                        .unwrap(),
                );
            }
        });
        if iteration >= WARMUP {
            samples.push(sample);
        }
    }
    report("dag_poll_snapshot_list_history_x200", POLLS, &samples);
}

#[test]
#[ignore = "perf bench; run explicitly"]
fn perf_bench_hooks() {
    use maho_ext_api::InputSource;
    use maho_omo_ultrawork::generated_directive::SENPI_ULTRAWORK_DIRECTIVE;
    use maho_omo_ultrawork::{ArmingSnapshot, classify_ultrawork_input, is_ultrawork_input, select_directive_text};

    const CALLS: u64 = 20_000;
    let en = "Please refactor the scheduler so the journal stops cloning the checkpoint on every append, then run the tests and report the results back to me. ".repeat(12);
    let ko = "스케줄러를 리팩터링해서 저널이 append 할 때마다 체크포인트를 복제하지 않게 하고 테스트를 돌려서 결과를 알려줘. ".repeat(12);
    let en_keyword = format!("{en} ulw");
    let cases: [(&str, &str); 3] = [("en_plain", &en), ("ko_plain", &ko), ("en_keyword", &en_keyword)];
    let bench = |name: String, f: &dyn Fn()| {
        let mut samples = Vec::new();
        for iteration in 0..9 {
            let (sample, _) = measure(|| {
                for _ in 0..CALLS {
                    f();
                }
            });
            if iteration >= 2 {
                samples.push(sample);
            }
        }
        report(&name, CALLS, &samples);
    };
    for (label, text) in cases {
        bench(format!("ultrawork_classify_{label}"), &|| {
            black_box(classify_ultrawork_input(black_box(text), InputSource::Interactive, ArmingSnapshot::default()));
        });
        bench(format!("ultrawork_is_input_{label}"), &|| {
            black_box(is_ultrawork_input(black_box(text)));
        });
        bench(format!("ultrawork_select_directive_{label}"), &|| {
            black_box(select_directive_text(black_box(text), SENPI_ULTRAWORK_DIRECTIVE));
        });
        bench(format!("mass_ulw_detect_{label}"), &|| {
            black_box(maho_omo_mass_ulw::is_mass_ulw_input(black_box(text)));
            black_box(maho_omo_mass_ulw::skill_already_invoked(black_box(text)));
        });
    }
}
