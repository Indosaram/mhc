use maho_codemode::{
    bridges::output_bridge::OutputExecuteTool,
    extension::session_manager::{CodemodeSessionManager, CreateCodemodeSessionManagerOptions},
    tool::types::EvalKernelRunInput,
};
use serde_json::json;
use std::{collections::HashMap, sync::Arc, time::Duration};

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

static PROBE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct ProbeEnvCleanup;
impl Drop for ProbeEnvCleanup {
    fn drop(&mut self) {
        maho_ext_api::process_env::delete("MAHO_PROBE_HOST_TOKEN");
    }
}

async fn probe(passthrough: Option<Vec<String>>) -> String {
    let _lock = PROBE_LOCK.lock().await;
    let _cleanup = ProbeEnvCleanup;
    maho_ext_api::process_env::set("MAHO_PROBE_HOST_TOKEN", "host-secret");
    let root = tempfile::tempdir().expect("root");
    let settings = maho_codemode::config::settings::CodemodeSettings {
        env_passthrough: passthrough,
        ..Default::default()
    };
    let session = CodemodeSessionManager::start(CreateCodemodeSessionManagerOptions {
        session_id: "env-scrub".into(),
        cwd: root.path().into(),
        settings,
        local_roots: None,
        artifacts_dir: Some(root.path().into()),
        session_env: Some(HashMap::from([
            ("NPM_TOKEN".into(), "overlay-secret".into()),
            ("KEEP_VAR".into(), "kept".into()),
        ])),
        executor: Arc::new(Executor),
        list_tools: None,
        complete: Arc::new(|_| Box::pin(async { Ok(json!({})) })),
    })
    .await
    .expect("session");
    let kernel = session.get_javascript_kernel().await.expect("kernel");
    let result = tokio::time::timeout(
        Duration::from_secs(15),
        kernel.run_eval(EvalKernelRunInput {
            cell_id: "probe".into(),
            code: "[String(process.env.NPM_TOKEN), String(process.env.KEEP_VAR), String(process.env.MAHO_PROBE_HOST_TOKEN)].join('|')".into(),
            ..Default::default()
        }),
    )
    .await
    .expect("bounded")
    .expect("cell");
    let repr = result["valueRepr"].as_str().unwrap_or_default().to_owned();
    session.dispose().await.expect("dispose");
    repr
}

#[tokio::test]
async fn session_overlay_secret_is_scrubbed_from_the_eval_child() {
    let repr = probe(Some(Vec::new())).await;
    assert_eq!(repr, "\"undefined|kept|undefined\"", "unexpected probe output");
}

#[tokio::test]
async fn passthrough_keeps_the_overlay_secret_in_the_eval_child() {
    let repr = probe(Some(vec!["NPM_TOKEN".into()])).await;
    assert_eq!(repr, "\"overlay-secret|kept|undefined\"", "unexpected probe output");
}

#[tokio::test]
async fn unset_passthrough_leaves_the_eval_environment_untouched() {
    let repr = probe(None).await;
    assert_eq!(repr, "\"overlay-secret|kept|host-secret\"", "unexpected probe output");
}

#[tokio::test]
async fn host_env_secret_is_scrubbed_from_the_eval_child() {
    let repr = probe(Some(Vec::new())).await;
    assert_eq!(repr, "\"undefined|kept|undefined\"", "unexpected probe output");
}

#[tokio::test]
async fn passthrough_keeps_a_host_env_secret_in_the_eval_child() {
    let repr = probe(Some(vec!["MAHO_PROBE_HOST_TOKEN".into()])).await;
    assert_eq!(repr, "\"undefined|kept|host-secret\"", "unexpected probe output");
}
