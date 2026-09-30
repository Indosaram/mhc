//! `dag/graph.test.ts`.
// allow: SIZE_OK - one-to-one translation of graph.test.ts.

use pretty_assertions::assert_eq;

use super::*;

const AT: &str = "2026-01-01T00:00:00.000Z";

fn node(id: &str, depends_on: &[&str]) -> DagNodeInput {
    DagNodeInput {
        id: id.to_string(),
        prompt: format!("do {id}"),
        target: DagNodeTarget::Category("quick".to_string()),
        label: None,
        depends_on: (!depends_on.is_empty())
            .then(|| depends_on.iter().map(ToString::to_string).collect()),
        task_summary: None,
        description: None,
        load_skills: None,
    }
}

fn definition(nodes: Vec<DagNodeInput>) -> DagDefinition {
    DagDefinition {
        key: "run-key".to_string(),
        name: "example".to_string(),
        nodes,
    }
}

fn compile(definition: &DagDefinition) -> DagCompileResult {
    compile_dag(
        definition,
        &DagCompileOptions {
            at: Some(AT.to_string()),
            settings: None,
        },
    )
}

fn diamond() -> DagDefinition {
    definition(vec![
        node("plan", &[]),
        node("build", &["plan"]),
        node("test", &["plan"]),
        node("synthesize", &["build", "test"]),
    ])
}

fn ids(values: &[&str]) -> Vec<String> {
    values.iter().map(ToString::to_string).collect()
}

fn wave_ids(result: &DagCompileResult) -> Vec<Vec<String>> {
    result
        .waves
        .iter()
        .map(|wave| wave.node_ids.clone())
        .collect()
}

fn codes(result: &DagCompileResult) -> Vec<DagCompileErrorCode> {
    result.errors.iter().map(|error| error.code).collect()
}

fn edge(from: &str, to: &str) -> DagEdge {
    DagEdge {
        from: from.to_string(),
        to: to.to_string(),
    }
}

fn numbered(count: usize) -> Vec<DagNodeInput> {
    (0..count)
        .map(|index| node(&format!("n{index:03}"), &[]))
        .collect()
}

// compileDag structure

#[test]
fn given_a_four_node_diamond_when_compiled_then_waves_follow_fan_out_then_join() {
    let result = compile(&diamond());

    assert!(result.ok);
    assert_eq!(
        wave_ids(&result),
        vec![
            ids(&["plan"]),
            ids(&["build", "test"]),
            ids(&["synthesize"])
        ]
    );
    let indexes: Vec<usize> = result.waves.iter().map(|wave| wave.index).collect();
    assert_eq!(indexes, vec![0, 1, 2]);
    assert_eq!(result.diagnostics, Vec::new());
}

#[test]
fn given_a_diamond_when_compiled_then_edges_derive_from_depends_on_only() {
    assert_eq!(
        compile(&diamond()).edges,
        vec![
            edge("plan", "build"),
            edge("plan", "test"),
            edge("build", "synthesize"),
            edge("test", "synthesize"),
        ]
    );
}

#[test]
fn given_nodes_with_routes_and_labels_when_compiled_then_nodes_carry_pending_state_and_the_submitted_prompt()
 {
    let with_targets = definition(vec![
        DagNodeInput {
            label: Some("Alpha".to_string()),
            task_summary: Some("sum".to_string()),
            description: Some("desc".to_string()),
            load_skills: Some(ids(&["programming"])),
            ..node("a", &[])
        },
        DagNodeInput {
            prompt: "run b".to_string(),
            target: DagNodeTarget::SubagentType {
                subagent_type: "momus".to_string(),
                model: Some("gpt-5.6".to_string()),
            },
            ..node("b", &["a"])
        },
    ]);

    let result = compile(&with_targets);

    let first = &result.nodes[0];
    assert_eq!(
        (
            first.id.as_str(),
            first.label.as_deref(),
            first.prompt.as_str()
        ),
        ("a", Some("Alpha"), "do a")
    );
    assert_eq!(
        first.route,
        DagRoute::Category {
            category: "quick".to_string()
        }
    );
    assert_eq!(
        (
            first.depends_on.clone(),
            first.state,
            first.attempt,
            first.created_at.as_str()
        ),
        (Vec::new(), DagNodeState::Pending, 0, AT)
    );
    let second = &result.nodes[1];
    assert_eq!((second.id.as_str(), second.prompt.as_str()), ("b", "run b"));
    assert_eq!(
        second.route,
        DagRoute::Agent {
            agent: "momus".to_string(),
            model: Some("gpt-5.6".to_string())
        }
    );
    assert_eq!(
        (second.depends_on.clone(), second.state),
        (ids(&["a"]), DagNodeState::Pending)
    );
}

#[test]
fn given_a_downstream_node_when_compiled_then_the_prompt_is_never_templated_with_upstream_ids() {
    let templated = definition(vec![
        node("plan", &[]),
        DagNodeInput {
            prompt: "use {{plan}}".to_string(),
            ..node("build", &["plan"])
        },
    ]);

    assert_eq!(compile(&templated).nodes[1].prompt, "use {{plan}}");
}

// compileDag ordering determinism

#[test]
fn given_same_wave_nodes_declared_out_of_id_order_when_compiled_then_wave_order_follows_declaration_index_then_id()
 {
    let declared = definition(vec![
        node("zeta", &[]),
        node("alpha", &[]),
        node("gamma", &["zeta"]),
        node("beta", &["zeta"]),
    ]);

    assert_eq!(
        wave_ids(&compile(&declared)),
        vec![ids(&["zeta", "alpha"]), ids(&["gamma", "beta"])]
    );
}

#[test]
fn given_identical_graphs_declared_in_different_node_order_when_compiled_then_wave_membership_is_identical()
 {
    let sorted = |result: &DagCompileResult| -> Vec<Vec<String>> {
        wave_ids(result)
            .into_iter()
            .map(|mut wave| {
                wave.sort();
                wave
            })
            .collect()
    };
    let forward = compile(&diamond());
    let backward = compile(&definition(vec![
        node("synthesize", &["build", "test"]),
        node("test", &["plan"]),
        node("build", &["plan"]),
        node("plan", &[]),
    ]));

    assert_eq!(sorted(&backward), sorted(&forward));
}

// compileDag critical path

#[test]
fn given_a_longest_chain_when_compiled_then_the_critical_path_is_that_chain_in_dependency_order() {
    let chained = definition(vec![
        node("a", &[]),
        node("b", &["a"]),
        node("c", &["b"]),
        node("d", &["c"]),
        node("e", &["a"]),
    ]);

    assert_eq!(compile(&chained).critical_path, ids(&["a", "b", "c", "d"]));
}

#[test]
fn given_two_equally_long_chains_when_compiled_then_the_tie_breaks_lexicographically_on_the_full_id_sequence()
 {
    let tied = definition(vec![
        node("root", &[]),
        node("m1", &["root"]),
        node("m2", &["m1"]),
        node("b1", &["root"]),
        node("b2", &["b1"]),
    ]);

    assert_eq!(compile(&tied).critical_path, ids(&["root", "b1", "b2"]));
}

// compileDag bottlenecks

fn bottleneck(node_id: &str, blocked_count: usize) -> DagBottleneck {
    DagBottleneck {
        node_id: node_id.to_string(),
        blocked_count,
    }
}

#[test]
fn given_a_diamond_when_compiled_then_bottlenecks_count_transitive_descendants_sorted_by_blocked_count_then_id()
 {
    assert_eq!(
        compile(&diamond()).bottlenecks,
        vec![
            bottleneck("plan", 3),
            bottleneck("build", 1),
            bottleneck("test", 1),
            bottleneck("synthesize", 0),
        ]
    );
}

#[test]
fn given_a_deep_chain_when_compiled_then_transitive_descendants_are_counted_not_direct_dependents()
{
    let chain = definition(vec![node("a", &[]), node("b", &["a"]), node("c", &["b"])]);

    assert_eq!(compile(&chain).bottlenecks[0], bottleneck("a", 2));
}

// compileDag validation

#[test]
fn given_duplicate_node_ids_when_compiled_then_rejected_naming_the_duplicate_id_and_no_graph_produced()
 {
    let result = compile(&definition(vec![node("a", &[]), node("a", &[])]));

    assert!(!result.ok);
    assert!(result.nodes.is_empty() && result.waves.is_empty());
    assert_eq!(codes(&result), vec![DagCompileErrorCode::DuplicateNodeId]);
    assert_eq!(result.errors[0].node_ids, ids(&["a"]));
    let DagDiagnostic::NodeFlag {
        node_id,
        message,
        at,
    } = &result.diagnostics[0]
    else {
        panic!("expected node_flag, got {:?}", result.diagnostics[0]);
    };
    assert_eq!((node_id.as_str(), at.as_str()), ("a", AT));
    assert!(message.contains('a'));
}

#[test]
fn given_an_unknown_dependency_when_compiled_then_rejected_naming_the_missing_id() {
    let result = compile(&definition(vec![node("a", &[]), node("b", &["ghost"])]));

    assert!(!result.ok);
    assert_eq!(codes(&result), vec![DagCompileErrorCode::UnknownDependency]);
    assert_eq!(result.errors[0].node_ids, ids(&["b", "ghost"]));
    let DagDiagnostic::RunFlag { message, .. } = &result.diagnostics[0] else {
        panic!("expected run_flag");
    };
    assert!(message.contains("ghost"));
}

#[test]
fn given_a_self_dependency_when_compiled_then_rejected_as_a_self_dependency_not_a_cycle() {
    let result = compile(&definition(vec![node("a", &["a"])]));

    assert!(!result.ok);
    assert_eq!(codes(&result), vec![DagCompileErrorCode::SelfDependency]);
    assert_eq!(result.errors[0].node_ids, ids(&["a"]));
}

#[test]
fn given_a_cyclic_three_node_graph_when_compiled_then_rejected_listing_the_exact_cycle_members_deterministically()
 {
    let cyclic = definition(vec![
        node("c", &["b"]),
        node("b", &["a"]),
        node("a", &["c"]),
        node("free", &[]),
    ]);

    let result = compile(&cyclic);

    assert!(!result.ok);
    assert_eq!(codes(&result), vec![DagCompileErrorCode::Cycle]);
    assert_eq!(result.errors[0].node_ids, ids(&["a", "b", "c", "a"]));
    // QA (task 41): pinned against the value computed directly from the pinned TS
    // `compileDag` (see .omo/evidence/task-41-cycle.txt for the TS command/output).
    assert_eq!(
        result.errors[0].message,
        "dependency cycle detected: a -> b -> c -> a"
    );
    let DagDiagnostic::RunFlag { message, at } = &result.diagnostics[0] else {
        panic!("expected run_flag");
    };
    assert_eq!(at, AT);
    assert!(message.contains("a -> b -> c -> a"));
}

#[test]
fn given_the_identical_cycle_fixture_from_the_pinned_ts_when_compiled_then_rejected_with_the_ts_error_text()
 {
    // QA (task 41): the identical three-node fixture the pinned TS `compileDag` was run with; the
    // TS command, output and rc are recorded in .omo/evidence/task-41-cycle.txt.
    let cyclic = DagDefinition {
        key: "cycle-fixture".to_string(),
        name: "cyclic".to_string(),
        nodes: vec![node("a", &["c"]), node("b", &["a"]), node("c", &["b"])],
    };

    let result = compile(&cyclic);

    assert!(!result.ok);
    assert_eq!(codes(&result), vec![DagCompileErrorCode::Cycle]);
    assert_eq!(
        result.errors[0].message,
        "dependency cycle detected: a -> b -> c -> a"
    );
    assert_eq!(result.errors[0].node_ids, ids(&["a", "b", "c", "a"]));
}

#[test]
fn given_the_same_cyclic_graph_declared_in_a_different_order_when_compiled_then_the_cycle_listing_is_identical()
 {
    let first = compile(&definition(vec![
        node("c", &["b"]),
        node("b", &["a"]),
        node("a", &["c"]),
    ]));
    let second = compile(&definition(vec![
        node("a", &["c"]),
        node("c", &["b"]),
        node("b", &["a"]),
    ]));

    assert_eq!(second.errors[0].node_ids, first.errors[0].node_ids);
}

// compileDag size bounds

#[test]
fn given_65_nodes_when_compiled_with_default_settings_then_rejected_on_max_nodes_per_run() {
    let result = compile(&definition(numbered(65)));

    assert!(!result.ok);
    assert_eq!(codes(&result), vec![DagCompileErrorCode::NodeCountExceeded]);
    assert!(result.errors[0].message.contains("65"));
    assert!(result.errors[0].message.contains("64"));
}

#[test]
fn given_64_nodes_when_compiled_with_default_settings_then_accepted() {
    assert!(compile(&definition(numbered(64))).ok);
}

#[test]
fn given_a_prompt_over_max_prompt_bytes_when_compiled_then_rejected_naming_the_node() {
    let oversized = definition(vec![DagNodeInput {
        prompt: "x".repeat(262_145),
        ..node("a", &[])
    }]);

    let result = compile(&oversized);

    assert!(!result.ok);
    assert_eq!(
        codes(&result),
        vec![DagCompileErrorCode::PromptBytesExceeded]
    );
    assert_eq!(result.errors[0].node_ids, ids(&["a"]));
}

#[test]
fn given_a_multibyte_prompt_when_measured_then_bytes_are_counted_not_code_units() {
    let with_prompt = |prompt: String| {
        definition(vec![DagNodeInput {
            prompt,
            ..node("a", &[])
        }])
    };

    assert!(!compile(&with_prompt("é".repeat(131_073))).ok);
    assert!(compile(&with_prompt("é".repeat(131_072))).ok);
}

#[test]
fn given_a_depends_on_fan_out_over_max_nodes_per_run_when_compiled_then_rejected_on_fan_out() {
    let mut nodes: Vec<DagNodeInput> = (0..4)
        .map(|index| node(&format!("u{index}"), &[]))
        .collect();
    nodes.push(node("join", &["u0", "u1", "u2", "u3"]));

    let result = compile_dag(
        &definition(nodes),
        &DagCompileOptions {
            at: Some(AT.to_string()),
            settings: Some(DagSettings {
                max_nodes_per_run: 3,
                ..DAG_SETTINGS_DEFAULTS
            }),
        },
    );

    assert!(!result.ok);
    let fanout = result
        .errors
        .iter()
        .find(|error| error.code == DagCompileErrorCode::DependencyFanoutExceeded)
        .expect("fan-out error");
    assert_eq!(fanout.node_ids, ids(&["join"]));
}

// compileDag purity

#[test]
fn given_the_same_definition_compiled_twice_when_serialized_then_output_is_identical() {
    let serialize = |result: DagCompileResult| {
        serde_json::to_string(&(
            result.nodes,
            result.edges,
            result.waves,
            result.critical_path,
            result.bottlenecks,
            result.diagnostics,
        ))
        .expect("json")
    };

    assert_eq!(
        serialize(compile(&diamond())),
        serialize(compile(&diamond()))
    );
}
