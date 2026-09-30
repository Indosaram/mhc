//! `dag/fingerprint.test.ts`.

use pretty_assertions::assert_eq;
use serde_json::json;

use super::*;

fn category_route() -> DagRoute {
    DagRoute::Category {
        category: "quick".to_string(),
    }
}

fn agent_route() -> DagRoute {
    DagRoute::Agent {
        agent: "omo".to_string(),
        model: Some("gpt-5.6".to_string()),
    }
}

fn base_node() -> DagNodeFingerprintInputV1 {
    DagNodeFingerprintInputV1 {
        node_id: "a".to_string(),
        label: "Node A".to_string(),
        depends_on: vec!["b".to_string(), "c".to_string()],
        prompt: "summarize the repo".to_string(),
        route: category_route(),
        task_summary: None,
        description: None,
        child_name: "child-a".to_string(),
    }
}

fn definition(nodes: Vec<DagNodeFingerprintInputV1>) -> DagDefinitionFingerprintInputV1 {
    DagDefinitionFingerprintInputV1 {
        name: "example".to_string(),
        scheduler: DAG_SCHEDULER_CONTRACT,
        nodes,
    }
}

// dagFingerprint canonicalization

#[test]
fn given_key_order_differs_when_fingerprinted_then_identical_hashes() {
    let a = dag_fingerprint(&json!({ "alpha": 1, "beta": 2, "nested": { "y": 1, "x": 2 } }));
    let b = dag_fingerprint(&json!({ "nested": { "x": 2, "y": 1 }, "beta": 2, "alpha": 1 }));

    assert_eq!(a, b);
    assert_eq!(a.len(), 64);
    assert!(
        a.chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    );
}

#[test]
fn given_absent_optional_fields_when_fingerprinted_then_omitted_as_if_absent() {
    let with_none = dag_definition_fingerprint(&definition(vec![DagNodeFingerprintInputV1 {
        task_summary: None,
        ..base_node()
    }]));
    let without = dag_definition_fingerprint(&definition(vec![base_node()]));

    assert_eq!(with_none, without);
}

#[test]
fn given_arrays_with_different_order_when_fingerprinted_then_different_hashes() {
    assert_ne!(
        dag_fingerprint(&json!([1, 2, 3])),
        dag_fingerprint(&json!([3, 2, 1]))
    );
}

#[test]
fn given_strings_differing_by_whitespace_only_when_fingerprinted_then_different_hashes() {
    assert_ne!(
        dag_fingerprint(&json!("hello world")),
        dag_fingerprint(&json!("hello  world"))
    );
    assert_ne!(
        dag_fingerprint(&json!("hello")),
        dag_fingerprint(&json!("hello\n"))
    );
}

#[test]
fn given_a_known_object_when_fingerprinted_then_it_hashes_the_sorted_key_canonical_json() {
    let expected = format!("{:x}", Sha256::digest(br#"{"a":1,"b":[true,null,"x"]}"#));

    assert_eq!(
        dag_fingerprint(&json!({ "b": [true, null, "x"], "a": 1 })),
        expected
    );
}

// dagDefinitionFingerprint

#[test]
fn given_reordered_depends_on_when_fingerprinted_then_identical_fingerprints() {
    let reordered = definition(vec![DagNodeFingerprintInputV1 {
        depends_on: vec!["c".to_string(), "b".to_string()],
        ..base_node()
    }]);

    assert_eq!(
        dag_definition_fingerprint(&reordered),
        dag_definition_fingerprint(&definition(vec![base_node()]))
    );
}

#[test]
fn given_reordered_node_list_when_fingerprinted_then_identical_fingerprints() {
    let other = DagNodeFingerprintInputV1 {
        node_id: "z".to_string(),
        label: "Node Z".to_string(),
        depends_on: Vec::new(),
        prompt: "second task".to_string(),
        route: agent_route(),
        task_summary: None,
        description: None,
        child_name: "child-z".to_string(),
    };

    let forward = dag_definition_fingerprint(&definition(vec![base_node(), other.clone()]));
    let backward = dag_definition_fingerprint(&definition(vec![other, base_node()]));

    assert_eq!(forward, backward);
}

#[test]
fn given_a_one_character_change_in_the_original_prompt_when_fingerprinted_then_different_fingerprints()
 {
    let changed = definition(vec![DagNodeFingerprintInputV1 {
        prompt: "summarize the repO".to_string(),
        ..base_node()
    }]);

    assert_ne!(
        dag_definition_fingerprint(&changed),
        dag_definition_fingerprint(&definition(vec![base_node()]))
    );
}

#[test]
fn given_semantically_identical_definitions_built_independently_when_fingerprinted_then_identical()
{
    let rebuilt = DagDefinitionFingerprintInputV1 {
        nodes: vec![DagNodeFingerprintInputV1 {
            child_name: "child-a".to_string(),
            node_id: "a".to_string(),
            label: "Node A".to_string(),
            depends_on: vec!["b".to_string(), "c".to_string()],
            route: DagRoute::Category {
                category: "quick".to_string(),
            },
            prompt: "summarize the repo".to_string(),
            task_summary: None,
            description: None,
        }],
        scheduler: DagSchedulerContract {
            dependency_data: "filesystem-only",
            failure_policy: "continue-independent",
            wave_admission: "strict-barrier",
        },
        name: "example".to_string(),
    };

    assert_eq!(
        dag_definition_fingerprint(&rebuilt),
        dag_definition_fingerprint(&definition(vec![base_node()]))
    );
}

#[test]
fn given_the_same_submitted_definition_across_a_skill_file_change_on_disk_when_fingerprinted_twice_then_identical()
 {
    let dir = tempfile::tempdir().expect("tempdir");
    let skill = dir.path().join("skill.md");
    std::fs::write(&skill, "# skill v1").expect("write v1");
    let before = dag_definition_fingerprint(&definition(vec![base_node()]));

    std::fs::write(&skill, "# skill v2 with different content and digest").expect("write v2");
    let after = dag_definition_fingerprint(&definition(vec![base_node()]));

    assert_eq!(after, before);
}

#[test]
fn given_optional_node_fields_present_when_fingerprinted_then_included_in_the_hash() {
    let with_optionals = definition(vec![DagNodeFingerprintInputV1 {
        task_summary: Some("short".to_string()),
        description: Some("long".to_string()),
        ..base_node()
    }]);

    assert_ne!(
        dag_definition_fingerprint(&with_optionals),
        dag_definition_fingerprint(&definition(vec![base_node()]))
    );
}

// nodeFingerprintInput

#[test]
fn given_a_node_input_with_unsorted_depends_on_when_normalized_then_sorts_depends_on_and_keeps_the_original_prompt()
 {
    let normalized = node_fingerprint_input(&DagNodeFingerprintInputV1 {
        node_id: "n".to_string(),
        label: "L".to_string(),
        depends_on: vec!["d".to_string(), "a".to_string()],
        prompt: "exact prompt  as submitted".to_string(),
        route: agent_route(),
        task_summary: None,
        description: None,
        child_name: "child-n".to_string(),
    });

    assert_eq!(
        normalized.depends_on,
        vec!["a".to_string(), "d".to_string()]
    );
    assert_eq!(normalized.prompt, "exact prompt  as submitted");
}

// QA (task 41): cross-check against a value computed directly from the pinned TS
// `dagDefinitionFingerprint` (see .omo/evidence/task-41-dag.txt for the TS command/output).

#[test]
fn dag_fingerprint_matches_ts_five_node_fixture() {
    let nodes = vec![
        DagNodeFingerprintInputV1 {
            node_id: "a".to_string(),
            label: "A".to_string(),
            depends_on: Vec::new(),
            prompt: "do a".to_string(),
            route: DagRoute::Category {
                category: "quick".to_string(),
            },
            task_summary: None,
            description: None,
            child_name: "a".to_string(),
        },
        DagNodeFingerprintInputV1 {
            node_id: "b".to_string(),
            label: "B".to_string(),
            depends_on: vec!["a".to_string()],
            prompt: "do b".to_string(),
            route: DagRoute::Category {
                category: "quick".to_string(),
            },
            task_summary: None,
            description: None,
            child_name: "b".to_string(),
        },
        DagNodeFingerprintInputV1 {
            node_id: "c".to_string(),
            label: "C".to_string(),
            depends_on: vec!["a".to_string()],
            prompt: "do c".to_string(),
            route: DagRoute::Agent {
                agent: "explore".to_string(),
                model: None,
            },
            task_summary: None,
            description: None,
            child_name: "c".to_string(),
        },
        DagNodeFingerprintInputV1 {
            node_id: "d".to_string(),
            label: "D".to_string(),
            depends_on: vec!["b".to_string(), "c".to_string()],
            prompt: "do d".to_string(),
            route: DagRoute::Agent {
                agent: "explore".to_string(),
                model: Some("claude-sonnet-5".to_string()),
            },
            task_summary: None,
            description: None,
            child_name: "d".to_string(),
        },
        DagNodeFingerprintInputV1 {
            node_id: "e".to_string(),
            label: "E".to_string(),
            depends_on: vec!["d".to_string()],
            prompt: "do e".to_string(),
            route: DagRoute::Category {
                category: "unspecified-low".to_string(),
            },
            task_summary: None,
            description: None,
            child_name: "e".to_string(),
        },
    ];

    let fp = dag_definition_fingerprint(&DagDefinitionFingerprintInputV1 {
        name: "fixture-5-node".to_string(),
        scheduler: DAG_SCHEDULER_CONTRACT,
        nodes,
    });

    assert_eq!(
        fp,
        "2700d97cc05dc6f2f5b8ae41977321ab4754760f0fd4d45aebcc883f9ff37d07"
    );
}
