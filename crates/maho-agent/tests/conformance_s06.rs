//! Port of senpi `packages/agent/test/conformance/{archive,case-registry,loop-core-boundary,run-suite}.test.ts`
//! and `packages/agent/test/execution/batch-store-entry.test.ts`.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use maho_agent::agent::Agent as LoopCoreAgent;
use maho_agent::conformance::loop_core_manifest::{LOOP_CORE_EXTERNAL_MODULES, LOOP_CORE_MODULES};
use maho_agent::conformance::mapping::{
    ConformanceManifest, ManifestCase, MappingProblem, MappingProblemCode, build_manifest,
    find_implementation_gaps, find_mapping_problems,
};
use maho_agent::conformance::registry::{AdapterKind, CONFORMANCE_REGISTRY, RegisteredCase};
use maho_agent::conformance::runner::{CaseImplementation, CaseResult, ConformanceHost, run_suite};
use maho_agent::conformance::suite_hash::{SuiteHashEntry, compute_suite_hash, sha256_hex};
use maho_agent::durable::testing::kill_store::kill_store_cases;
use maho_agent::durable::testing::kill_wake::kill_wake_cases;
use maho_agent::durable::testing::multi_host_cases::multi_host_cases;
use maho_agent::durable::testing::store_cases::store_cases;
use maho_agent::durable::testing::store_harness::StoreCaseAdapters;
use maho_agent::durable::testing::wake_cases::wake_cases;
use maho_agent::durable::testing::wake_harness::WakeCaseAdapters;
use maho_agent::execution::canonical_digest::canonical_json;
use maho_agent::execution::testing::batch_cases::batch_cases;
use maho_agent::execution::testing::batch_executor::BatchCaseAdapters;
use maho_agent::execution::testing::cancel_cases::cancel_cases;
use maho_agent::execution::testing::consent_cases::consent_cases;
use maho_agent::execution::testing::epoch_cases::epoch_cases;
use maho_agent::execution::testing::fault_hand_cases::fault_hand_cases;
use maho_agent::execution::testing::hand_harness::HandCaseAdapters;
use maho_agent::execution::testing::kill_batch::kill_batch_cases;
use maho_agent::execution::testing::kill_epoch::kill_epoch_cases;
use maho_agent::execution::testing::kill_hand::kill_hand_cases;
use maho_agent::execution::testing::kill_hook::kill_hook_cases;
use maho_agent::execution::testing::kill_ledger::kill_ledger_cases;
use maho_agent::execution::testing::kill_nest::kill_nest_cases;
use maho_agent::execution::testing::kill_wakex::kill_wakex_cases;
use maho_agent::execution::testing::ledger_cases::ledger_cases;
use maho_agent::execution::testing::lifecycle_cases::lifecycle_cases;
use maho_agent::execution::testing::multi_epoch_cases::multi_epoch_cases;
use maho_agent::execution::testing::multi_hand_cases::multi_hand_cases;
use maho_agent::execution::testing::multi_lifecycle_cases::multi_lifecycle_cases;
use maho_agent::execution::testing::multi_seq_cases::multi_seq_cases;
use maho_agent::execution::testing::nest_cases::nest_cases;
use maho_agent::execution::testing::scope_cases::scope_cases;
use maho_agent::execution::testing::seq_cases::seq_cases;
use maho_agent::execution::testing::wake_exec_cases::wake_exec_cases;
use maho_agent::execution::tool_host::{
    BatchStoreMissingError, ExecutionPipelineWitness, PreflightCarryMissingError,
    no_preflight_carry_fn,
};
use maho_agent::transport::testing::fault_cases::fault_transport_cases;
use maho_agent::transport::testing::multi_cases::multi_cases;
use maho_agent::transport::testing::transport_cases::transport_cases;
use maho_agent::transport::testing::transport_harness::TransportCaseAdapters;
use maho_agent::{
    Agent, AgentContext, AgentLoopConfig, AgentOptions, AgentTool, AgentToolResult,
    PartialAgentState, agent_loop, agent_loop_continue, identity_convert_to_llm,
    open_memory_batch_store, run_agent_loop, run_agent_loop_continue,
};
use serde_json::json;

use crate::s06_adapters::{
    DEFAULT_T0, batch_case_adapters, hand_case_adapters, memory_store_adapter,
    memory_wake_case_adapters, transport_case_adapters,
};
use crate::support::{
    assistant, collect_events, scripted_stream_fn, test_model, text_block, tool_call, user_message,
};

fn senpi_root() -> PathBuf {
    if let Ok(path) = std::env::var("SENPI_SRC") {
        let pb = PathBuf::from(path);
        if pb.exists() {
            return pb;
        }
    }
    PathBuf::from("/home/gwjeon/work/upstream/senpi-pin")
}

async fn read_repo_file(rel: &str) -> Result<Vec<u8>, String> {
    let primary = senpi_root().join(rel);
    match tokio::fs::read(&primary).await {
        Ok(bytes) => Ok(bytes),
        Err(primary_err) => {
            let fallback = PathBuf::from("/home/gwjeon/work/upstream/senpi").join(rel);
            tokio::fs::read(&fallback)
                .await
                .map_err(|_| format!("failed to read {}: {primary_err}", primary.display()))
        }
    }
}

fn manifest_case(id: &str, adapters: Vec<AdapterKind>) -> ManifestCase {
    ManifestCase {
        id: id.to_owned(),
        impl_path: "packages/agent/test/conformance/run-suite.test.ts".to_owned(),
        impl_hash: "f".repeat(64),
        intro: "#1".to_owned(),
        adapters,
        expected: "an expected outcome".to_owned(),
    }
}

#[test]
fn conformance_registry_has_unique_ids_and_complete_mapping() {
    let registry = CONFORMANCE_REGISTRY();
    assert!(!registry.is_empty());
    assert_eq!(
        find_mapping_problems(&registry),
        Vec::<MappingProblem>::new()
    );
}

#[tokio::test]
async fn conformance_manifest_hashes_every_impl_and_computes_suite_hash() {
    let registry = CONFORMANCE_REGISTRY();
    let manifest = build_manifest(&registry, |path| {
        let path = path.to_owned();
        async move { read_repo_file(&path).await }
    })
    .await
    .expect("build manifest against SENPI_SRC");
    let ids: Vec<&str> = manifest.cases.iter().map(|c| c.id.as_str()).collect();
    let reg_ids: Vec<&str> = registry.iter().map(|c| c.id).collect();
    assert_eq!(ids, reg_ids);
    assert_eq!(manifest.suite_hash.len(), 64);
    assert!(
        manifest
            .suite_hash
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    );
    let recomputed = compute_suite_hash(
        &manifest
            .cases
            .iter()
            .map(|c| SuiteHashEntry {
                id: c.id.clone(),
                impl_hash: c.impl_hash.clone(),
            })
            .collect::<Vec<_>>(),
    )
    .expect("suite hash");
    assert_eq!(manifest.suite_hash, recomputed);
}

#[test]
fn find_mapping_problems_detects_every_malformed_entry() {
    let valid = RegisteredCase {
        id: "STORE-01",
        impl_path: "packages/agent/src/conformance/registry.ts",
        intro: "#1",
        adapters: &[AdapterKind::Store],
        expected: "outcome",
    };
    let check = |cases: Vec<RegisteredCase>, expected: Vec<MappingProblemCode>| {
        let codes: Vec<MappingProblemCode> = find_mapping_problems(&cases)
            .into_iter()
            .map(|p| p.code)
            .collect();
        assert_eq!(codes, expected);
    };
    check(vec![valid, valid], vec![MappingProblemCode::DuplicateId]);
    check(
        vec![RegisteredCase { id: "", ..valid }],
        vec![MappingProblemCode::MalformedId],
    );
    check(
        vec![RegisteredCase {
            id: "store-01",
            ..valid
        }],
        vec![MappingProblemCode::MalformedId],
    );
    check(
        vec![RegisteredCase {
            impl_path: "/abs/cases.ts",
            ..valid
        }],
        vec![MappingProblemCode::MalformedImpl],
    );
    check(
        vec![RegisteredCase {
            impl_path: "packages/../cases.ts",
            ..valid
        }],
        vec![MappingProblemCode::MalformedImpl],
    );
    check(
        vec![RegisteredCase {
            impl_path: "packages/agent/cases.js",
            ..valid
        }],
        vec![MappingProblemCode::MalformedImpl],
    );
    check(
        vec![RegisteredCase {
            intro: " ",
            ..valid
        }],
        vec![MappingProblemCode::MissingIntro],
    );
    check(
        vec![RegisteredCase {
            adapters: &[],
            ..valid
        }],
        vec![MappingProblemCode::MissingAdapters],
    );
    check(
        vec![RegisteredCase {
            adapters: &[AdapterKind::Store, AdapterKind::Wake, AdapterKind::Store],
            ..valid
        }],
        vec![MappingProblemCode::DuplicateAdapter],
    );
    check(
        vec![RegisteredCase {
            expected: "",
            ..valid
        }],
        vec![MappingProblemCode::MissingExpected],
    );
}

#[test]
fn canonical_json_and_suite_hash_are_order_independent() {
    let a = json!({ "b": 2, "a": { "d": true, "c": [1, 2] } });
    let b = json!({ "a": { "c": [1, 2], "d": true }, "b": 2 });
    assert_eq!(canonical_json(&a), canonical_json(&b));
    assert_eq!(sha256_hex(b"hello"), sha256_hex(b"hello"));

    let e1 = SuiteHashEntry {
        id: "A-01".to_owned(),
        impl_hash: "1".repeat(64),
    };
    let e2 = SuiteHashEntry {
        id: "B-01".to_owned(),
        impl_hash: "2".repeat(64),
    };
    assert_eq!(
        compute_suite_hash(&[e1.clone(), e2.clone()]).expect("h1"),
        compute_suite_hash(&[e2, e1]).expect("h2")
    );
}

#[tokio::test]
async fn run_suite_reports_pass_fail_missing_impl_and_missing_adapter_in_manifest_order() {
    let manifest = ConformanceManifest {
        suite_hash: "0".repeat(64),
        cases: vec![
            manifest_case("STORE-01", vec![AdapterKind::Store]),
            manifest_case("STORE-02", vec![AdapterKind::Store]),
            manifest_case("STORE-03", vec![AdapterKind::Store]),
            manifest_case("WAKE-01", vec![AdapterKind::Store, AdapterKind::Wake]),
        ],
    };
    let calls: Arc<std::sync::Mutex<Vec<String>>> = Arc::default();
    let host = ConformanceHost {
        name: "fake".to_owned(),
        provides: vec![AdapterKind::Store],
        adapters: calls.clone(),
    };
    let mut impls: BTreeMap<&'static str, CaseImplementation<Arc<std::sync::Mutex<Vec<String>>>>> =
        BTreeMap::new();
    impls.insert(
        "STORE-01",
        Arc::new(|calls| {
            Box::pin(async move {
                calls.lock().expect("calls").push("STORE-01".to_owned());
                Ok(())
            })
        }),
    );
    impls.insert(
        "STORE-02",
        Arc::new(|_| Box::pin(async move { Err("commit lost".to_owned()) })),
    );
    impls.insert(
        "WAKE-01",
        Arc::new(|calls| {
            Box::pin(async move {
                calls.lock().expect("calls").push("WAKE-01".to_owned());
                Ok(())
            })
        }),
    );

    let gaps = find_implementation_gaps(&CONFORMANCE_REGISTRY()[..0], &impls);
    assert_eq!(gaps.unregistered.len(), 3);

    let report = run_suite(&manifest, &host, &impls).await;
    assert_eq!(
        report.results,
        vec![
            CaseResult::Pass {
                id: "STORE-01".to_owned()
            },
            CaseResult::Fail {
                id: "STORE-02".to_owned(),
                error: "commit lost".to_owned(),
            },
            CaseResult::Fail {
                id: "STORE-03".to_owned(),
                error: "no implementation is registered for this case".to_owned(),
            },
            CaseResult::Fail {
                id: "WAKE-01".to_owned(),
                error: "the host does not provide adapter: wake".to_owned(),
            },
        ]
    );
    assert_eq!(*calls.lock().expect("calls"), vec!["STORE-01".to_owned()]);
}

#[tokio::test]
async fn full_store_and_ledger_conformance_suite_passes() {
    let mut impls: BTreeMap<&'static str, CaseImplementation<StoreCaseAdapters>> = BTreeMap::new();
    impls.extend(store_cases());
    impls.extend(kill_store_cases());
    impls.extend(ledger_cases());
    impls.extend(kill_ledger_cases());
    let subset: Vec<RegisteredCase> = CONFORMANCE_REGISTRY()
        .into_iter()
        .filter(|c| impls.contains_key(c.id))
        .collect();
    assert_eq!(
        find_implementation_gaps(&subset, &impls).unimplemented,
        Vec::<String>::new()
    );
    let manifest = build_manifest(&subset, |path| {
        let path = path.to_owned();
        async move { read_repo_file(&path).await }
    })
    .await
    .expect("manifest");
    let report = run_suite(
        &manifest,
        &ConformanceHost {
            name: "memory".to_owned(),
            provides: vec![AdapterKind::Store],
            adapters: StoreCaseAdapters {
                store: memory_store_adapter(),
            },
        },
        &impls,
    )
    .await;
    let failures: Vec<_> = report
        .results
        .into_iter()
        .filter(|r| !matches!(r, CaseResult::Pass { .. }))
        .collect();
    assert_eq!(failures, Vec::<CaseResult>::new());
}

#[tokio::test]
async fn full_wake_conformance_suite_passes() {
    let mut impls: BTreeMap<&'static str, CaseImplementation<WakeCaseAdapters>> = BTreeMap::new();
    impls.extend(wake_cases());
    impls.extend(multi_host_cases());
    impls.extend(kill_wake_cases());
    let subset: Vec<RegisteredCase> = CONFORMANCE_REGISTRY()
        .into_iter()
        .filter(|c| impls.contains_key(c.id))
        .collect();
    assert_eq!(
        find_implementation_gaps(&subset, &impls).unimplemented,
        Vec::<String>::new()
    );
    let manifest = build_manifest(&subset, |path| {
        let path = path.to_owned();
        async move { read_repo_file(&path).await }
    })
    .await
    .expect("manifest");
    let report = run_suite(
        &manifest,
        &ConformanceHost {
            name: "memory".to_owned(),
            provides: vec![AdapterKind::Store, AdapterKind::Wake],
            adapters: memory_wake_case_adapters(DEFAULT_T0),
        },
        &impls,
    )
    .await;
    let failures: Vec<_> = report
        .results
        .into_iter()
        .filter(|r| !matches!(r, CaseResult::Pass { .. }))
        .collect();
    assert_eq!(failures, Vec::<CaseResult>::new());
}

#[tokio::test]
async fn full_transport_and_hook_conformance_suite_passes() {
    let mut impls: BTreeMap<&'static str, CaseImplementation<TransportCaseAdapters>> =
        BTreeMap::new();
    impls.extend(transport_cases());
    impls.extend(multi_cases());
    impls.extend(fault_transport_cases());
    impls.extend(kill_hook_cases());
    let subset: Vec<RegisteredCase> = CONFORMANCE_REGISTRY()
        .into_iter()
        .filter(|c| impls.contains_key(c.id))
        .collect();
    assert_eq!(
        find_implementation_gaps(&subset, &impls).unimplemented,
        Vec::<String>::new()
    );
    let manifest = build_manifest(&subset, |path| {
        let path = path.to_owned();
        async move { read_repo_file(&path).await }
    })
    .await
    .expect("manifest");
    let report = run_suite(
        &manifest,
        &ConformanceHost {
            name: "reference-transport".to_owned(),
            provides: vec![AdapterKind::Transport, AdapterKind::Executor],
            adapters: transport_case_adapters("linux"),
        },
        &impls,
    )
    .await;
    let failures: Vec<_> = report
        .results
        .into_iter()
        .filter(|r| !matches!(r, CaseResult::Pass { .. }))
        .collect();
    assert_eq!(failures, Vec::<CaseResult>::new());
}

#[tokio::test]
async fn full_batch_coordinator_conformance_suite_passes() {
    let mut impls: BTreeMap<&'static str, CaseImplementation<BatchCaseAdapters>> = BTreeMap::new();
    impls.extend(batch_cases());
    impls.extend(kill_batch_cases());
    impls.extend(seq_cases());
    impls.extend(multi_seq_cases());
    impls.extend(consent_cases());
    impls.extend(nest_cases());
    impls.extend(cancel_cases());
    impls.extend(kill_nest_cases());
    impls.extend(wake_exec_cases());
    impls.extend(kill_wakex_cases());
    impls.extend(lifecycle_cases());
    impls.extend(scope_cases());
    impls.extend(multi_lifecycle_cases());
    let subset: Vec<RegisteredCase> = CONFORMANCE_REGISTRY()
        .into_iter()
        .filter(|c| impls.contains_key(c.id))
        .collect();
    assert_eq!(
        find_implementation_gaps(&subset, &impls).unimplemented,
        Vec::<String>::new()
    );
    let manifest = build_manifest(&subset, |path| {
        let path = path.to_owned();
        async move { read_repo_file(&path).await }
    })
    .await
    .expect("manifest");
    let report = run_suite(
        &manifest,
        &ConformanceHost {
            name: "memory-batch".to_owned(),
            provides: vec![AdapterKind::Store, AdapterKind::Executor],
            adapters: batch_case_adapters(),
        },
        &impls,
    )
    .await;
    let failures: Vec<_> = report
        .results
        .into_iter()
        .filter(|r| !matches!(r, CaseResult::Pass { .. }))
        .collect();
    assert_eq!(failures, Vec::<CaseResult>::new());
}

#[tokio::test]
async fn full_hand_and_epoch_conformance_suite_passes() {
    let mut impls: BTreeMap<&'static str, CaseImplementation<HandCaseAdapters>> = BTreeMap::new();
    impls.extend(kill_hand_cases());
    impls.extend(multi_hand_cases());
    impls.extend(fault_hand_cases());
    impls.extend(epoch_cases());
    impls.extend(multi_epoch_cases());
    impls.extend(kill_epoch_cases());
    let subset: Vec<RegisteredCase> = CONFORMANCE_REGISTRY()
        .into_iter()
        .filter(|c| impls.contains_key(c.id))
        .collect();
    assert_eq!(
        find_implementation_gaps(&subset, &impls).unimplemented,
        Vec::<String>::new()
    );
    let manifest = build_manifest(&subset, |path| {
        let path = path.to_owned();
        async move { read_repo_file(&path).await }
    })
    .await
    .expect("manifest");
    let report = run_suite(
        &manifest,
        &ConformanceHost {
            name: "memory-hand".to_owned(),
            provides: vec![
                AdapterKind::Store,
                AdapterKind::Transport,
                AdapterKind::Executor,
            ],
            adapters: hand_case_adapters("linux"),
        },
        &impls,
    )
    .await;
    let failures: Vec<_> = report
        .results
        .into_iter()
        .filter(|r| !matches!(r, CaseResult::Pass { .. }))
        .collect();
    assert_eq!(failures, Vec::<CaseResult>::new());
}

#[test]
fn loop_core_manifest_includes_core_and_excludes_node_defaults() {
    assert!(LOOP_CORE_MODULES.contains(&"agent-loop.ts"));
    assert!(LOOP_CORE_MODULES.contains(&"execution/tool-host.ts"));
    assert!(!LOOP_CORE_MODULES.contains(&"public-entry.ts"));
    assert!(!LOOP_CORE_MODULES.contains(&"durable/store-memory.ts"));
    assert!(!LOOP_CORE_MODULES.contains(&"execution/prepared/memory-batch-store.ts"));
    assert!(!LOOP_CORE_MODULES.contains(&"harness/env/nodejs.ts"));
    assert!(!LOOP_CORE_EXTERNAL_MODULES.contains(&"node:fs"));
}

fn echo_tool() -> AgentTool {
    AgentTool {
        label: "echo".to_owned(),
        prepare_arguments: None,
        replay: None,
        execution_mode: None,
        tool: maho_ai::types::Tool {
            name: "echo".to_owned(),
            description: "echo".to_owned(),
            parameters: json!({ "type": "object", "properties": { "n": { "type": "number" } } }),
            output_schema: None,
            freeform: None,
            constrained_sampling: None,
        },
        execute: Arc::new(|_, params, _, _| {
            Box::pin(async move { AgentToolResult::text(params.to_string()) })
        }),
    }
}

#[tokio::test]
async fn public_entry_runs_agent_and_loop_functions_with_no_factory_argument() {
    let witnesses: Arc<std::sync::Mutex<Vec<ExecutionPipelineWitness>>> = Arc::default();
    let w_sink = witnesses.clone();
    let agent = Agent::new(AgentOptions {
        initial_state: Some(PartialAgentState {
            model: Some(test_model()),
            tools: Some(vec![echo_tool()]),
            ..Default::default()
        }),
        stream_fn: Some(scripted_stream_fn(vec![
            assistant(
                vec![tool_call("c0", "echo", json!({ "n": 0 }))],
                maho_ai::types::StopReason::ToolUse,
            ),
            assistant(vec![text_block("done")], maho_ai::types::StopReason::Stop),
        ])),
        on_execution_pipeline: Some(Arc::new(move |witness| {
            let w_sink = w_sink.clone();
            Box::pin(async move {
                w_sink.lock().expect("witnesses").push(witness);
            })
        })),
        ..Default::default()
    });
    agent.prompt(maho_agent::AgentPromptInput::text("go")).await;
    assert_eq!(
        *witnesses.lock().expect("witnesses"),
        vec![ExecutionPipelineWitness {
            pipeline: "prepared",
            tool_call_id: "c0".to_owned(),
        }]
    );
    assert_eq!(
        agent
            .state()
            .messages()
            .iter()
            .filter(|m| m.role() == "toolResult")
            .count(),
        1
    );

    let run_witnesses: Arc<std::sync::Mutex<Vec<ExecutionPipelineWitness>>> = Arc::default();
    let rw_sink = run_witnesses.clone();
    let mut cfg = AgentLoopConfig::new(test_model(), identity_convert_to_llm());
    cfg.on_execution_pipeline = Some(Arc::new(move |witness| {
        let rw_sink = rw_sink.clone();
        Box::pin(async move {
            rw_sink.lock().expect("witnesses").push(witness);
        })
    }));
    let stream = agent_loop(
        vec![user_message("go")],
        AgentContext {
            system_prompt: String::new(),
            system_prompt_stable_prefix: None,
            messages: Vec::new(),
            tools: Some(vec![echo_tool()]),
            declared_tools: None,
        },
        cfg.clone(),
        None,
        Some(scripted_stream_fn(vec![
            assistant(
                vec![tool_call("c1", "echo", json!({ "n": 1 }))],
                maho_ai::types::StopReason::ToolUse,
            ),
            assistant(vec![text_block("done")], maho_ai::types::StopReason::Stop),
        ])),
    );
    let _ = collect_events(&stream).await;

    let stream2 = agent_loop_continue(
        AgentContext {
            system_prompt: String::new(),
            system_prompt_stable_prefix: None,
            messages: vec![user_message("go")],
            tools: Some(vec![echo_tool()]),
            declared_tools: None,
        },
        cfg.clone(),
        None,
        Some(scripted_stream_fn(vec![
            assistant(
                vec![tool_call("c2", "echo", json!({ "n": 2 }))],
                maho_ai::types::StopReason::ToolUse,
            ),
            assistant(vec![text_block("done")], maho_ai::types::StopReason::Stop),
        ])),
    );
    let _ = collect_events(&stream2).await;

    let sink: maho_agent::AgentEventSink = Arc::new(|_| Box::pin(async {}));
    run_agent_loop(
        vec![user_message("go")],
        AgentContext {
            system_prompt: String::new(),
            system_prompt_stable_prefix: None,
            messages: Vec::new(),
            tools: Some(vec![echo_tool()]),
            declared_tools: None,
        },
        cfg.clone(),
        sink.clone(),
        None,
        Some(scripted_stream_fn(vec![
            assistant(
                vec![tool_call("c3", "echo", json!({ "n": 3 }))],
                maho_ai::types::StopReason::ToolUse,
            ),
            assistant(vec![text_block("done")], maho_ai::types::StopReason::Stop),
        ])),
    )
    .await
    .expect("public_entry run_agent_loop");

    run_agent_loop_continue(
        AgentContext {
            system_prompt: String::new(),
            system_prompt_stable_prefix: None,
            messages: vec![user_message("go")],
            tools: Some(vec![echo_tool()]),
            declared_tools: None,
        },
        cfg,
        sink,
        None,
        Some(scripted_stream_fn(vec![
            assistant(
                vec![tool_call("c4", "echo", json!({ "n": 4 }))],
                maho_ai::types::StopReason::ToolUse,
            ),
            assistant(vec![text_block("done")], maho_ai::types::StopReason::Stop),
        ])),
    )
    .await
    .expect("public_entry run_agent_loop_continue");

    let ids: Vec<String> = run_witnesses
        .lock()
        .expect("witnesses")
        .iter()
        .map(|w| w.tool_call_id.clone())
        .collect();
    assert_eq!(ids, vec!["c1", "c2", "c3", "c4"]);
}

#[tokio::test]
async fn loop_core_alone_refuses_missing_batch_store_and_missing_after_tool_freeze() {
    let agent = LoopCoreAgent::new_core(AgentOptions {
        initial_state: Some(PartialAgentState {
            model: Some(test_model()),
            tools: Some(vec![echo_tool()]),
            ..Default::default()
        }),
        stream_fn: Some(scripted_stream_fn(vec![assistant(
            vec![tool_call("c0", "echo", json!({}))],
            maho_ai::types::StopReason::ToolUse,
        )])),
        ..Default::default()
    });
    agent.prompt(maho_agent::AgentPromptInput::text("go")).await;
    let failed = agent
        .state()
        .messages()
        .iter()
        .filter_map(|m| m.as_assistant())
        .find(|m| m.error_message.is_some())
        .cloned()
        .expect("terminal failure message");
    assert_eq!(
        failed.error_message.as_deref(),
        Some(BatchStoreMissingError.to_string().as_str())
    );
    assert!(
        !agent
            .state()
            .messages()
            .iter()
            .any(|m| m.role() == "toolResult")
    );

    let agent2 = LoopCoreAgent::new_core(AgentOptions {
        initial_state: Some(PartialAgentState {
            model: Some(test_model()),
            tools: Some(vec![echo_tool()]),
            ..Default::default()
        }),
        stream_fn: Some(scripted_stream_fn(vec![assistant(
            vec![tool_call("c0", "echo", json!({}))],
            maho_ai::types::StopReason::ToolUse,
        )])),
        batch_store_factory: Some(open_memory_batch_store()),
        ..Default::default()
    });
    agent2
        .prompt(maho_agent::AgentPromptInput::text("go"))
        .await;
    let failed2 = agent2
        .state()
        .messages()
        .iter()
        .filter_map(|m| m.as_assistant())
        .find(|m| m.error_message.is_some())
        .cloned()
        .expect("terminal failure message");
    assert_eq!(
        failed2.error_message.as_deref(),
        Some(PreflightCarryMissingError.to_string().as_str())
    );
    assert!(
        !agent2
            .state()
            .messages()
            .iter()
            .any(|m| m.role() == "toolResult")
    );
}

#[tokio::test]
async fn loop_core_opens_a_fresh_store_for_every_batch() {
    let opens = Arc::new(AtomicUsize::new(0));
    let opens_clone = opens.clone();
    let agent = LoopCoreAgent::new_core(AgentOptions {
        initial_state: Some(PartialAgentState {
            model: Some(test_model()),
            tools: Some(vec![echo_tool()]),
            ..Default::default()
        }),
        stream_fn: Some(scripted_stream_fn(vec![
            assistant(
                vec![tool_call("same-id", "echo", json!({ "n": 0 }))],
                maho_ai::types::StopReason::ToolUse,
            ),
            assistant(
                vec![tool_call("same-id", "echo", json!({ "n": 1 }))],
                maho_ai::types::StopReason::ToolUse,
            ),
            assistant(vec![text_block("done")], maho_ai::types::StopReason::Stop),
        ])),
        after_tool_freeze: Some(no_preflight_carry_fn()),
        batch_store_factory: Some(Arc::new(move || {
            opens_clone.fetch_add(1, Ordering::SeqCst);
            (open_memory_batch_store())()
        })),
        ..Default::default()
    });
    agent.prompt(maho_agent::AgentPromptInput::text("go")).await;
    assert_eq!(opens.load(Ordering::SeqCst), 2);
    let results: Vec<bool> = agent
        .state()
        .messages()
        .iter()
        .filter_map(|m| m.as_tool_result())
        .map(|tr| tr.is_error)
        .collect();
    assert_eq!(results, vec![false, false]);
}
