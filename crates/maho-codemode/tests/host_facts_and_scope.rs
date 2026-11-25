use maho_codemode::{
    bridges::{output_bridge::OutputExecuteTool, schema_bridge::EvalSchemaToolInfo},
    extension::{
        host_facts::{host_line_in_raw, model_id_from, pinned_display_path_in_raw},
        kernel_preludes::eval_description_key,
        reachable_tools::{ReachableToolRegistry, eval_only_tool_names, reachable_tool_names},
        runtime_factory::top_level_only_tool_refusal,
        scope_kernels::{KernelScope, KernelScopeOwned, ScopedKernelManagers},
        session_manager::{CodemodeSessionManager, CreateCodemodeSessionManagerOptions},
    },
    tool::types::EvalRuntimeInfo,
};
use maho_tools::definition::{KernelPrelude, ToolExposure};
use serde_json::json;
use std::{collections::HashSet, sync::Arc};

struct MockRegistry {
    active: Vec<String>,
    all: Vec<EvalSchemaToolInfo>,
}

impl ReachableToolRegistry for MockRegistry {
    fn get_active_tools(&self) -> Result<Vec<String>, String> {
        Ok(self.active.clone())
    }

    fn get_all_tools(&self) -> Result<Vec<EvalSchemaToolInfo>, String> {
        Ok(self.all.clone())
    }
}

struct NoopExecutor;
impl OutputExecuteTool for NoopExecutor {
    fn execute_tool<'a>(
        &'a self,
        _: &'a str,
        _: serde_json::Value,
        _: maho_ext_api::ExecuteToolOptions,
    ) -> maho_ext_api::ExecuteToolFuture<'a> {
        Box::pin(async { Ok(maho_ext_api::AgentToolResult::text("ok")) })
    }
}

fn sample_runtime() -> EvalRuntimeInfo {
    EvalRuntimeInfo {
        name: "bun".into(),
        version: "1.3.10".into(),
        path: None,
        isolation: Some("process".into()),
    }
}

#[test]
fn host_line_uses_pinned_workstation_facts() {
    let line = host_line_in_raw(
        Some(&sample_runtime()),
        Some(r#"{"cpuModel":"Fixture CPU","cores":8,"kernel":"Linux 6.1"}"#),
    )
    .expect("valid workstation pin");
    assert!(line.contains("Fixture CPU"));
    assert!(line.contains("8 cores"));
    assert!(line.contains("bun 1.3.10"));
}

#[test]
fn workstation_pin_rejects_malformed_shapes() {
    let runtime = sample_runtime();
    for raw in [
        r#"{"cpuModel":"","cores":0,"kernel":"Linux"}"#,
        r#"{"cpuModel":"CPU","cores":4}"#,
        r#"{"cpuModel":"CPU","cores":4,"kernel":"Linux","repoRoot":{"from":"rel","to":"/abs"}}"#,
    ] {
        assert!(
            host_line_in_raw(Some(&runtime), Some(raw)).is_err(),
            "{raw}"
        );
    }
}

#[test]
fn pinned_display_path_maps_repo_root_prefix_only() {
    let pin = r#"{"cpuModel":"CPU","cores":4,"kernel":"Linux","repoRoot":{"from":"/real/repo","to":"/virtual/repo"}}"#;
    assert_eq!(
        pinned_display_path_in_raw("/real/repo/skill/bun/SKILL.md", Some(pin)).unwrap(),
        "/virtual/repo/skill/bun/SKILL.md"
    );
    assert_eq!(
        pinned_display_path_in_raw("/real/repo", Some(pin)).unwrap(),
        "/virtual/repo"
    );
    assert_eq!(
        pinned_display_path_in_raw("/other/path", Some(pin)).unwrap(),
        "/other/path"
    );
}

#[test]
fn model_id_from_extracts_nested_model_id() {
    assert_eq!(
        model_id_from(&json!({"model":{"id":"claude-sonnet"}})),
        Some("claude-sonnet".into())
    );
    assert_eq!(model_id_from(&json!({})), None);
}

#[test]
fn reachable_and_eval_only_tools_match_registration_order() {
    let registry = MockRegistry {
        active: vec!["read".into(), "eval".into()],
        all: vec![
            EvalSchemaToolInfo {
                name: "read".into(),
                description: None,
                parameters: Some(json!({})),
                exposure: Some(ToolExposure::Direct),
                eval_only: Some(false),
                kernel_prelude: None,
            },
            EvalSchemaToolInfo {
                name: "eval_only_tool".into(),
                description: Some("hidden".into()),
                parameters: Some(json!({})),
                exposure: Some(ToolExposure::Eval),
                eval_only: None,
                kernel_prelude: None,
            },
            EvalSchemaToolInfo {
                name: "explicit_eval".into(),
                description: None,
                parameters: Some(json!({})),
                exposure: Some(ToolExposure::Direct),
                eval_only: Some(true),
                kernel_prelude: None,
            },
        ],
    };

    let reachable = reachable_tool_names(&registry);
    assert_eq!(
        reachable,
        HashSet::from(["read".into(), "eval".into(), "eval_only_tool".into()])
    );
    assert_eq!(
        eval_only_tool_names(&registry),
        vec!["eval_only_tool".to_string(), "explicit_eval".to_string()]
    );
}

#[test]
fn eval_description_key_tracks_preludes_and_eval_only_tools_and_refusals() {
    let key_a = eval_description_key(
        &[KernelPrelude {
            documentation: "doc".into(),
            ..Default::default()
        }],
        &["a".into()],
    );
    let key_b = eval_description_key(
        &[KernelPrelude {
            documentation: "doc".into(),
            ..Default::default()
        }],
        &["b".into()],
    );
    assert_ne!(key_a, key_b);
    assert!(top_level_only_tool_refusal("show_html_page").is_some());
    assert!(top_level_only_tool_refusal("read").is_none());
}

#[tokio::test]
async fn scope_kernels_isolate_and_release_per_scope() {
    let dir = tempfile::tempdir().expect("tempdir");
    let base = CreateCodemodeSessionManagerOptions {
        session_id: "root".into(),
        owner_session_id: None,
        cwd: dir.path().to_path_buf(),
        settings: Default::default(),
        local_roots: None,
        artifacts_dir: Some(dir.path().join("artifacts")),
        session_env: None,
        inherited_env: None,
        kill_guard: false,
        executor: Arc::new(NoopExecutor),
        list_tools: None,
        complete: Arc::new(|_| Box::pin(async { Ok(json!({})) })),
    };

    let managers = ScopedKernelManagers::with_default_factory(base);
    let scope_a = KernelScope {
        id: "scope-a".into(),
        root: dir.path().to_string_lossy().into_owned(),
    };
    let scope_b = KernelScope {
        id: "scope-b".into(),
        root: dir.path().to_string_lossy().into_owned(),
    };

    let m_a1 = managers
        .for_scope(&scope_a, KernelScopeOwned::default())
        .await
        .expect("scope a1");
    let m_a2 = managers
        .for_scope(&scope_a, KernelScopeOwned::default())
        .await
        .expect("scope a2");
    let m_b = managers
        .for_scope(&scope_b, KernelScopeOwned::default())
        .await
        .expect("scope b");

    assert!(Arc::ptr_eq(&m_a1, &m_a2));
    assert!(!Arc::ptr_eq(&m_a1, &m_b));

    managers.release_scope("scope-a").await.expect("release a");
    let m_a3 = managers
        .for_scope(&scope_a, KernelScopeOwned::default())
        .await
        .expect("scope a3");
    assert!(!Arc::ptr_eq(&m_a1, &m_a3));

    managers.dispose().await.expect("dispose");
    let after_dispose = managers
        .for_scope(&scope_a, KernelScopeOwned::default())
        .await;
    assert!(after_dispose.is_err());
    let _: fn(CreateCodemodeSessionManagerOptions) -> _ = CodemodeSessionManager::start;
}
