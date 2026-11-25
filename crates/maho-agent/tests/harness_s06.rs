//! Port of senpi `packages/agent/test/harness/` S06 tests (`nodejs-env-listdir-race`,
//! `read-tree-sitter-runtime`, `skills-ignored-links`, `summary-request-session-cwd`, `toolhost-harness`).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use maho_agent::execution::tool_host::ExecutionPipelineWitness;
use maho_agent::harness::agent_harness::AgentHarnessOptions;
use maho_agent::harness::compaction::compaction::create_summary_request_options;
use maho_agent::harness::context::{BACKGROUND_CONTEXT, get_session_cwd, with_session_cwd};
use maho_agent::harness::env::nodejs::NodeExecutionEnv;
use maho_agent::harness::runtime::harness::create_agent_harness_with_options;
use maho_agent::harness::runtime::lane::PromptInput;
use maho_agent::harness::session::types::{Session, SessionMetadata};
use maho_agent::harness::session::{
    MemoryStorage, MemoryStorageOptions, StorageBackedSession, StorageBackedSessionOptions,
};
use maho_agent::harness::skills::load_skills;
use maho_agent::harness::types::{AgentHarnessTool, FileSystem};
use maho_agent::harness::utils::read_folders::{
    brace_scanner::scan_braces,
    tree_sitter::{
        engine::{TreeSitterFolderOptions, load_tree_sitter_folder},
        syntax::TreeSitterLanguage,
    },
    types::{READ_FOLD_SETTINGS, ReadBraceScan, ReadFolder, ReadFolderInput, ReadFolderResult},
};
use maho_agent::types::AgentToolResult;
use maho_ai::providers::faux::{
    FauxResponseStep, faux_assistant_message, faux_provider, faux_tool_call,
};
use maho_ai::types::{BoxFuture, SimpleStreamOptions, Tool};
use serde_json::json;

struct HarnessPublicAuth;

impl maho_ai::models::ModelsAuth for HarnessPublicAuth {
    fn resolve<'a>(
        &'a self,
        _: &'a dyn maho_ai::models::Provider,
        _: &'a maho_ai::models::AuthResolutionOverrides,
    ) -> BoxFuture<'a, Result<Option<maho_ai::models::AuthResolution>, maho_ai::models::ModelsError>>
    {
        Box::pin(async {
            Ok(Some(maho_ai::models::AuthResolution {
                ambient: false,
                auth: maho_ai::models::ProviderAuthResult {
                    api_key: Some("fixture".into()),
                    ..Default::default()
                },
                env: None,
            }))
        })
    }

    fn refresh_credential<'a>(
        &'a self,
        _: &'a dyn maho_ai::models::Provider,
        _: Option<&'a maho_ai::models::Credential>,
        _: &'a maho_ai::utils::abort::AbortSignal,
    ) -> BoxFuture<'a, Result<Option<maho_ai::models::Credential>, maho_ai::models::ModelsError>>
    {
        Box::pin(async { Ok(None) })
    }
}

#[tokio::test]
async fn nodejs_env_list_dir_handles_normal_entries_cleanly() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("present.txt"), "ok").expect("write");
    let env = NodeExecutionEnv::new(dir.path().to_string_lossy().into_owned());
    let listed = env
        .list_dir(".", &BACKGROUND_CONTEXT)
        .await
        .expect("list_dir");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].name, "present.txt");
}

#[test]
fn tree_sitter_runtime_loads_js_and_caches_or_falls_back() {
    struct BraceReadFolder;
    impl ReadFolder for BraceReadFolder {
        fn id(&self) -> &str {
            "brace"
        }
        fn version(&self) -> &str {
            "1"
        }
        fn fold(&self, input: ReadFolderInput<'_>) -> ReadFolderResult {
            match scan_braces(input.text, "javascript", input.settings) {
                ReadBraceScan::Parsed { .. } => ReadFolderResult::Parsed {
                    text: input.text.to_owned(),
                    ranges: Vec::new(),
                },
                ReadBraceScan::ParseFailure { reason } => ReadFolderResult::ParseFailure { reason },
            }
        }
    }
    let fallback: Arc<dyn ReadFolder> = Arc::new(BraceReadFolder);
    let folder = load_tree_sitter_folder(
        TreeSitterLanguage::Js,
        TreeSitterFolderOptions::new(fallback.clone()),
    )
    .expect("tree-sitter js loads");
    let src = "function alpha(x) {\n  const y = x + 1;\n  const z = y + 2;\n  const w = z + 3;\n  return w;\n}\n";
    let res = folder.fold(ReadFolderInput {
        path: "alpha.js",
        text: src,
        settings: READ_FOLD_SETTINGS,
    });
    assert!(matches!(
        res,
        ReadFolderResult::Parsed { .. } | ReadFolderResult::Unsupported { .. }
    ));

    let missing = load_tree_sitter_folder(
        TreeSitterLanguage::Js,
        TreeSitterFolderOptions {
            fallback,
            parse_budget_ms: 250,
            resolve_grammar: Some(Arc::new(|_| None)),
            cache: false,
        },
    );
    assert!(missing.is_none());
}

#[cfg(unix)]
#[tokio::test]
async fn skills_load_follows_symlinked_skill_directories_past_parent_gitignore() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    let target_skill = root.join("external_skill");
    std::fs::create_dir_all(&target_skill).expect("create target");
    std::fs::write(
        target_skill.join("SKILL.md"),
        "---\nname: linked-skill\ndescription: Linked\n---\nFollow link.\n",
    )
    .expect("write target SKILL.md");

    let skills_root = root.join(".agents/skills");
    std::fs::create_dir_all(&skills_root).expect("create skills root");
    std::fs::write(skills_root.join(".gitignore"), "linked/\n").expect("write .gitignore");
    std::os::unix::fs::symlink(&target_skill, skills_root.join("linked")).expect("symlink");

    let env = NodeExecutionEnv::new(root.to_string_lossy().into_owned());
    let loaded = load_skills(&env, &[".agents/skills"], &BACKGROUND_CONTEXT).await;
    assert!(
        loaded
            .skills
            .iter()
            .any(|skill| skill.name == "linked-skill")
    );
}

#[test]
fn summary_request_options_carries_session_cwd_when_set() {
    let ctx = with_session_cwd(Some("/work/project"), &BACKGROUND_CONTEXT);
    assert_eq!(get_session_cwd(&ctx).as_deref(), Some("/work/project"));
    let opts = create_summary_request_options(SimpleStreamOptions::default(), &ctx);
    assert_eq!(opts.stream.request.cwd.as_deref(), Some("/work/project"));

    let unset = create_summary_request_options(SimpleStreamOptions::default(), &BACKGROUND_CONTEXT);
    assert_eq!(unset.stream.request.cwd, None);
}

#[tokio::test]
async fn harness_tool_calls_run_on_prepared_tool_host_and_record_witness() {
    let session = Arc::new(StorageBackedSession::new(
        SessionMetadata {
            id: "toolhost-harness".into(),
            created_at: 1,
            storage_version: 1,
            cwd: Some("/tmp/p7-harness".into()),
            parent_session_id: None,
            legacy_parent_session_path: None,
            path: None,
        },
        Arc::new(MemoryStorage::new(MemoryStorageOptions::default())),
        StorageBackedSessionOptions::default(),
    ));
    session.attach();

    let faux = faux_provider(Default::default()).expect("faux");
    let models = maho_ai::models::create_models(Some(maho_ai::models::CreateModelsOptions {
        auth: Some(Arc::new(HarnessPublicAuth)),
        ..Default::default()
    }));
    models.set_provider(faux.provider.clone());

    let body_values: Arc<Mutex<Vec<String>>> = Arc::default();
    let body_sink = body_values.clone();
    let tool = Arc::new(AgentHarnessTool {
        label: "Probe".into(),
        prepare_arguments: None,
        replay: None,
        tool: Tool {
            name: "probe".into(),
            description: "Probe input".into(),
            parameters: json!({
                "type": "object",
                "properties": { "value": { "type": "string" } },
                "required": ["value"]
            }),
            output_schema: None,
            freeform: None,
            constrained_sampling: None,
        },
        execute: Arc::new(move |_, args, _, _: (), _, _| {
            let body_sink = body_sink.clone();
            Box::pin(async move {
                body_sink
                    .lock()
                    .expect("body_values")
                    .push(args["value"].as_str().unwrap_or_default().to_owned());
                Ok(AgentToolResult::text("ran"))
            })
        }),
    });

    let witnesses: Arc<Mutex<Vec<ExecutionPipelineWitness>>> = Arc::default();
    let w_sink = witnesses.clone();
    let (harness, _) = create_agent_harness_with_options(
        AgentHarnessOptions::<()> {
            session: session.clone(),
            models,
            model: faux.get_model(None).expect("faux model"),
            thinking_level: None,
            active_tool_names: Some(vec!["probe".into()]),
            tools: vec![tool],
            tool_context: Some(maho_agent::harness::agent_harness::ToolContextSource::Value(())),
            system_prompt: None,
            resources: None,
            stream_options: None,
            retry: None,
            compaction: None,
            steering_mode: None,
            follow_up_mode: None,
            tool_execution: None,
            to_provider_messages: None,
            entry_projectors: None,
            on_execution_pipeline: Some(Arc::new(move |witness| {
                let w_sink = w_sink.clone();
                Box::pin(async move {
                    w_sink.lock().expect("witnesses").push(witness);
                })
            })),
        },
        &BACKGROUND_CONTEXT,
    )
    .await
    .expect("harness creates");

    let lane = harness
        .lane("main", None, &BACKGROUND_CONTEXT)
        .await
        .expect("main lane");
    faux.set_responses(vec![
        FauxResponseStep::Message(Box::new(faux_assistant_message(
            vec![faux_tool_call(
                "probe",
                serde_json::Map::from_iter([("value".into(), json!("ran"))]),
                Some("call-1"),
            )],
            maho_ai::providers::faux::FauxAssistantMessageOptions {
                stop_reason: Some(maho_ai::types::StopReason::ToolUse),
                ..Default::default()
            },
        ))),
        FauxResponseStep::Message(Box::new(faux_assistant_message("done", Default::default()))),
    ]);

    let _outcome = tokio::time::timeout(
        Duration::from_secs(5),
        lane.prompt(
            PromptInput::Text {
                text: "go".into(),
                images: vec![],
            },
            &BACKGROUND_CONTEXT,
        ),
    )
    .await
    .expect("bounded prompt")
    .expect("prompt completes");

    assert_eq!(
        *body_values.lock().expect("body_values"),
        vec!["ran".to_owned()]
    );
    assert_eq!(
        *witnesses.lock().expect("witnesses"),
        vec![ExecutionPipelineWitness {
            pipeline: "prepared",
            tool_call_id: "call-1".to_owned(),
        }]
    );
    harness.close(&BACKGROUND_CONTEXT).await;
    session.close(&BACKGROUND_CONTEXT).await;
}
