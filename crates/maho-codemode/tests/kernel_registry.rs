use maho_codemode::{
    bridges::output_bridge::OutputExecuteTool,
    config::settings::CodemodeSettings,
    extension::{
        kernel_registration::java_script_kernel_memory,
        kernel_registry::{KERNEL_REGISTRY, KernelMeasure, KernelRegistration, KernelRegistry},
        session_manager::{CodemodeSessionManager, CreateCodemodeSessionManagerOptions},
    },
    kernels::js::context_manager::JavaScriptKernel,
    tool::types::{EvalKernelRunInput, EvalLanguage},
};
use serde_json::json;
use std::{future::Future, sync::Arc, time::Duration};

async fn bounded<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(15), future)
        .await
        .expect("bounded operation")
}

struct Executor;
impl OutputExecuteTool for Executor {
    fn execute_tool<'a>(
        &'a self,
        _: &'a str,
        _: serde_json::Value,
        _: maho_ext_api::ExecuteToolOptions,
    ) -> maho_ext_api::ExecuteToolFuture<'a> {
        Box::pin(async { Ok(maho_ext_api::AgentToolResult::text("fixture")) })
    }
}

async fn session(root: &std::path::Path, id: &str, process: bool) -> CodemodeSessionManager {
    let settings = serde_json::from_value::<CodemodeSettings>(json!({
        "isolation":{"js":if process {"process"} else {"worker"}},
        "memoryGcWatermarkBytes":1_000_000_000_000_u64,
        "memoryNoticeBytes":1_000_000_000_000_u64,
        "memoryCeilingBytes":1_000_000_000_000_u64
    }))
    .expect("settings");
    bounded(CodemodeSessionManager::start(
        CreateCodemodeSessionManagerOptions {
            session_id: id.into(),
            cwd: root.into(),
            settings,
            local_roots: None,
            artifacts_dir: Some(root.into()),
            session_env: None,
            executor: Arc::new(Executor),
            list_tools: None,
            complete: Arc::new(|_| Box::pin(async { Ok(json!({})) })),
        },
    ))
    .await
    .expect("session")
}

fn listing(session: &str) -> maho_codemode::extension::kernel_registry::KernelListing {
    let rows = KERNEL_REGISTRY
        .list()
        .into_iter()
        .filter(|row| row.session_id == session)
        .collect::<Vec<_>>();
    assert_eq!(rows.len(), 1);
    rows.into_iter().next().expect("registered kernel")
}

#[tokio::test]
async fn session_lists_warm_kernel_and_unregisters_on_dispose() {
    let root = tempfile::tempdir().expect("root");
    let id = uuid::Uuid::new_v4().to_string();
    let session = session(root.path(), &id, false).await;
    assert!(
        !KERNEL_REGISTRY
            .list()
            .iter()
            .any(|row| row.session_id == id)
    );
    let kernel = bounded(session.get_javascript_kernel())
        .await
        .expect("kernel");
    let initial = listing(&id);
    assert_eq!(initial.language, EvalLanguage::Js);
    assert_eq!(initial.measure, KernelMeasure::Heap);
    assert_eq!(initial.pid, None);
    assert!(!initial.busy);
    let warm = bounded(session.get_javascript_kernel())
        .await
        .expect("warm kernel");
    assert!(Arc::ptr_eq(&kernel, &warm));
    assert_eq!(listing(&id).id, initial.id);
    bounded(session.dispose()).await.expect("dispose");
    assert!(
        !KERNEL_REGISTRY
            .list()
            .iter()
            .any(|row| row.session_id == id)
    );
    assert!(
        bounded(KERNEL_REGISTRY.query(&initial.id))
            .await
            .expect("query")
            .is_none()
    );
}

#[tokio::test]
async fn fresh_queries_and_result_frames_update_cached_heap() {
    let kernel = Arc::new(
        bounded(JavaScriptKernel::start(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")),
            "registry-heap",
            2,
            None,
        ))
        .await
        .expect("kernel"),
    );
    let registry = KernelRegistry::default();
    registry.register(KernelRegistration {
        id: "heap".into(),
        session_id: "session".into(),
        language: EvalLanguage::Js,
        source: java_script_kernel_memory(kernel.clone()),
    });
    let reading = bounded(registry.query("heap"))
        .await
        .expect("query")
        .expect("reading");
    assert_eq!(reading.measure, KernelMeasure::Heap);
    assert!(reading.live_bytes > 0);
    assert_eq!(registry.last_live_bytes("heap"), Some(reading.live_bytes));
    let result = bounded(kernel.run_eval(EvalKernelRunInput {
        cell_id: "allocate".into(),
        code: "globalThis.registryBuffer = new Array(100000).fill('memory'); 42".into(),
        ..Default::default()
    }))
    .await
    .expect("cell");
    assert_eq!(
        registry.last_live_bytes("heap"),
        result["memory"]["liveBytes"].as_u64()
    );
    bounded(kernel.close()).await.expect("close");
    assert_eq!(registry.last_live_bytes("heap"), None);
    assert!(
        bounded(registry.query("heap"))
            .await
            .expect("closed query")
            .is_none()
    );
}

#[tokio::test]
async fn busy_listing_keeps_cached_reading_during_active_host_cell() {
    let kernel = Arc::new(
        bounded(JavaScriptKernel::start(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")),
            "registry-busy",
            2,
            None,
        ))
        .await
        .expect("kernel"),
    );
    let registry = KernelRegistry::default();
    registry.register(KernelRegistration {
        id: "busy".into(),
        session_id: "session".into(),
        language: EvalLanguage::Js,
        source: java_script_kernel_memory(kernel.clone()),
    });
    let (started, event) = tokio::sync::oneshot::channel();
    let (release, released) = tokio::sync::oneshot::channel();
    let running = kernel.clone();
    let cell = tokio::spawn(async move {
        running
            .run_eval(EvalKernelRunInput {
                cell_id: "host".into(),
                host_executor: Some(Box::new(move |_| {
                    Box::pin(async move {
                        started.send(()).expect("start event");
                        released.await.expect("release event");
                        json!({"type":"result","cellId":"host","ok":true,"durationMs":0})
                    })
                })),
                ..Default::default()
            })
            .await
    });
    bounded(event).await.expect("started");
    let fresh = bounded(registry.query("busy"))
        .await
        .expect("query")
        .expect("reading");
    let row = registry.list().pop().expect("listing");
    assert!(row.busy);
    assert_eq!(row.last_live_bytes, Some(fresh.live_bytes));
    release.send(()).expect("release");
    bounded(cell).await.expect("join").expect("cell");
    bounded(kernel.close()).await.expect("close");
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn process_registration_reports_footprint_and_child_pid() {
    let root = tempfile::tempdir().expect("root");
    let id = uuid::Uuid::new_v4().to_string();
    let session = session(root.path(), &id, true).await;
    let kernel = bounded(session.get_javascript_kernel())
        .await
        .expect("kernel");
    let row = listing(&id);
    assert_eq!(row.measure, KernelMeasure::Footprint);
    assert_eq!(row.pid, kernel.pid());
    assert!(row.pid.is_some());
    let reading = bounded(KERNEL_REGISTRY.query(&row.id))
        .await
        .expect("query")
        .expect("reading");
    assert_eq!(reading.measure, KernelMeasure::Footprint);
    assert!(reading.live_bytes > 0);
    assert_eq!(
        KERNEL_REGISTRY.last_live_bytes(&row.id),
        Some(reading.live_bytes)
    );
    bounded(session.dispose()).await.expect("dispose");
}
