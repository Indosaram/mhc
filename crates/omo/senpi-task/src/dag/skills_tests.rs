//! `dag/skills.test.ts`: createDagSkillMaterializer at run creation, and wired into DagManager.start.

use pretty_assertions::assert_eq;
use sha2::{Digest, Sha256};

use super::*;
use crate::dag::graph::{DagDefinition, DagNodeInput};
use crate::dag::manager::{
    DagManagerOptions, DagMaterializeSkillsInput, DagStartParams, create_dag_manager,
};
use crate::dag::store::{DagStoreConfig, DagStoreOptions, create_dag_file_store};
use crate::dag::types::{DagNodeState, DagNodeTarget, DagRunStatus};

const PARENT_SESSION_ID: &str = "parent-session";
const ROOT_SESSION_ID: &str = "root-session";
const RUN_ID: &str = "run-skills";
const AT: &str = "2025-01-01T00:00:00.000Z";

fn temp_dir() -> std::path::PathBuf {
    tempfile::tempdir().expect("tempdir").keep()
}

fn write_skill(cwd: &std::path::Path, name: &str, body: &str) {
    let dir = cwd.join(".senpi").join("skills").join(name);
    std::fs::create_dir_all(&dir).expect("create skill dir");
    std::fs::write(dir.join("SKILL.md"), body).expect("write skill body");
}

fn skill_prompt(cwd: &std::path::Path, name: &str, body: &str, prompt: &str) -> String {
    let skill_path = cwd.join(".senpi").join("skills").join(name).join("SKILL.md");
    let skill_dir = cwd.join(".senpi").join("skills").join(name);
    [
        format!("<skill name=\"{name}\" location=\"{}\">", skill_path.display()),
        format!("References are relative to {}.", skill_dir.display()),
        String::new(),
        body.to_string(),
        "</skill>".to_string(),
        String::new(),
        prompt.to_string(),
    ]
    .join("\n")
}

fn sha256_hex(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn node(id: &str, prompt: &str, load_skills: Option<Vec<&str>>) -> DagNodeInput {
    DagNodeInput {
        id: id.to_string(),
        prompt: prompt.to_string(),
        target: DagNodeTarget::Category("quick".to_string()),
        label: None,
        depends_on: None,
        task_summary: None,
        description: None,
        load_skills: load_skills.map(|names| names.into_iter().map(str::to_string).collect()),
    }
}

fn definition(nodes: Vec<DagNodeInput>) -> DagDefinition {
    DagDefinition {
        key: "run-key".to_string(),
        name: "skills run".to_string(),
        nodes,
    }
}

fn dag_store(project: &std::path::Path) -> std::sync::Arc<DagFileStore> {
    std::sync::Arc::new(
        create_dag_file_store(&DagStoreConfig::new(project), DagStoreOptions::default()).expect("dag store opens"),
    )
}

fn materializer(store: std::sync::Arc<DagFileStore>, cwd: &std::path::Path) -> DagMaterializeSkills {
    create_dag_skill_materializer(DagSkillMaterializerOptions {
        store,
        cwd: cwd.to_string_lossy().into_owned(),
        load_skills: None,
        home_dir: Some(temp_dir()),
        extra_dirs: Vec::new(),
    })
}

#[test]
fn given_a_node_requesting_a_present_skill_when_materialized_then_the_skill_block_precedes_the_original_prompt() {
    let project = temp_dir();
    let cwd = temp_dir();
    write_skill(&cwd, "programming", "programming skill body");
    let store = dag_store(&project);

    let result = materializer(std::sync::Arc::clone(&store), &cwd)(DagMaterializeSkillsInput {
        run_id: &RUN_ID.to_string(),
        definition: &definition(vec![node("build", "ship the feature", Some(vec!["programming"]))]),
        at: AT,
    });

    let effective_prompt = &result.nodes[0].1;
    let expected = skill_prompt(&cwd, "programming", "programming skill body", "ship the feature");
    assert_eq!(effective_prompt, &expected);
    assert!(effective_prompt.find("programming skill body") < effective_prompt.find("ship the feature"));
}

#[test]
fn given_a_resolved_skill_when_materialized_then_the_manifest_records_the_requested_names_the_sha256_digest_and_the_pinned_cwd()
 {
    let project = temp_dir();
    let cwd = temp_dir();
    let body = "git-master skill body\nwith two lines";
    write_skill(&cwd, "git-master", body);
    let store = dag_store(&project);

    materializer(std::sync::Arc::clone(&store), &cwd)(DagMaterializeSkillsInput {
        run_id: &RUN_ID.to_string(),
        definition: &definition(vec![node("commit", "commit it", Some(vec!["git-master"]))]),
        at: AT,
    });

    let manifest = read_dag_skill_manifest(&store, RUN_ID).expect("manifest exists");
    assert_eq!(manifest.cwd, cwd.to_string_lossy());
    let recorded = &manifest.nodes[0];
    assert_eq!(recorded.node_id, "commit");
    assert_eq!(recorded.requested, vec!["git-master".to_string()]);
    assert_eq!(
        recorded.resolved,
        vec![DagSkillDigestWire { name: "git-master".to_string(), sha256: sha256_hex(body) }]
    );
    assert_eq!(recorded.missing, Vec::<String>::new());
    assert_eq!(recorded.prompt, "commit it");
}

#[test]
fn given_a_node_requesting_an_absent_skill_when_materialized_then_a_missing_skill_diagnostic_is_recorded_and_the_prompt_still_dispatches()
 {
    let project = temp_dir();
    let cwd = temp_dir();
    write_skill(&cwd, "programming", "programming skill body");
    let store = dag_store(&project);

    let result = materializer(std::sync::Arc::clone(&store), &cwd)(DagMaterializeSkillsInput {
        run_id: &RUN_ID.to_string(),
        definition: &definition(vec![node("build", "ship the feature", Some(vec!["programming", "nonexistent"]))]),
        at: AT,
    });

    assert_eq!(result.diagnostics.len(), 1);
    let DagDiagnostic::MissingSkill { node_id, skill, message, at } = &result.diagnostics[0] else {
        panic!("expected missing_skill diagnostic");
    };
    assert_eq!(node_id, "build");
    assert_eq!(skill, "nonexistent");
    assert_eq!(message, "Skill \"nonexistent\" was not found.");
    assert_eq!(at, AT);
    let expected = skill_prompt(&cwd, "programming", "programming skill body", "ship the feature");
    assert_eq!(result.nodes[0].1, expected);
    assert_eq!(
        read_dag_skill_manifest(&store, RUN_ID).expect("manifest exists").nodes[0].missing,
        vec!["nonexistent".to_string()]
    );
}

#[test]
fn given_a_materialized_run_when_skill_md_is_rewritten_on_disk_then_the_persisted_effective_prompt_still_carries_the_creation_time_content()
 {
    let project = temp_dir();
    let cwd = temp_dir();
    write_skill(&cwd, "programming", "v1 skill body");
    let store = dag_store(&project);
    let materialize = materializer(std::sync::Arc::clone(&store), &cwd);
    materialize(DagMaterializeSkillsInput {
        run_id: &RUN_ID.to_string(),
        definition: &definition(vec![node("build", "ship the feature", Some(vec!["programming"]))]),
        at: AT,
    });

    write_skill(&cwd, "programming", "v2 skill body");

    let manifest = read_dag_skill_manifest(&store, RUN_ID).expect("manifest exists");
    let persisted = &manifest.nodes[0];
    assert!(persisted.effective_prompt.contains("v1 skill body"));
    assert!(!persisted.effective_prompt.contains("v2 skill body"));
    assert_eq!(
        persisted.resolved,
        vec![DagSkillDigestWire { name: "programming".to_string(), sha256: sha256_hex("v1 skill body") }]
    );
}

#[test]
fn given_a_node_without_load_skills_when_materialized_then_the_effective_prompt_is_the_untouched_original() {
    let project = temp_dir();
    let cwd = temp_dir();
    let store = dag_store(&project);

    let result = materializer(std::sync::Arc::clone(&store), &cwd)(DagMaterializeSkillsInput {
        run_id: &RUN_ID.to_string(),
        definition: &definition(vec![node("plain", "no skills here", None)]),
        at: AT,
    });

    assert_eq!(result.nodes, vec![("plain".to_string(), "no skills here".to_string())]);
    assert_eq!(result.diagnostics.len(), 0);
}

#[test]
fn given_a_run_created_with_a_materializer_when_the_cwd_loses_the_skill_afterwards_then_the_persisted_definition_keeps_the_creation_time_effective_prompt()
 {
    let project = temp_dir();
    let cwd = temp_dir();
    write_skill(&cwd, "programming", "v1 skill body");
    let store = dag_store(&project);
    let manager = create_dag_manager(DagManagerOptions {
        store: std::sync::Arc::clone(&store),
        new_run_id: None,
        now: None,
        materialize_skills: Some(materializer(std::sync::Arc::clone(&store), &cwd)),
        settings: None,
    });
    let started = manager
        .start(DagStartParams {
            definition: definition(vec![node("build", "ship the feature", Some(vec!["programming"]))]),
            parent_session_id: PARENT_SESSION_ID.to_string(),
            root_session_id: ROOT_SESSION_ID.to_string(),
        })
        .expect("start");

    std::fs::remove_dir_all(cwd.join(".senpi")).expect("remove skill dir");

    let record = manager.record(&started.snapshot.run_id, PARENT_SESSION_ID).expect("record");
    let expected = skill_prompt(&cwd, "programming", "v1 skill body", "ship the feature");
    assert_eq!(record.definition.nodes[0].effective_prompt, expected);
    assert_eq!(record.definition.nodes[0].prompt, "ship the feature");
}

#[test]
fn given_a_missing_skill_when_the_run_is_created_then_the_run_exists_with_a_missing_skill_diagnostic_and_no_failure() {
    let project = temp_dir();
    let cwd = temp_dir();
    let store = dag_store(&project);
    let manager = create_dag_manager(DagManagerOptions {
        store: std::sync::Arc::clone(&store),
        new_run_id: None,
        now: None,
        materialize_skills: Some(materializer(std::sync::Arc::clone(&store), &cwd)),
        settings: None,
    });

    let started = manager
        .start(DagStartParams {
            definition: definition(vec![node("build", "ship the feature", Some(vec!["nonexistent"]))]),
            parent_session_id: PARENT_SESSION_ID.to_string(),
            root_session_id: ROOT_SESSION_ID.to_string(),
        })
        .expect("start");

    assert_eq!(started.snapshot.status, DagRunStatus::Pending);
    let found = started.snapshot.diagnostics.iter().any(|diagnostic| {
        matches!(
            diagnostic,
            DagDiagnostic::MissingSkill { node_id, skill, message, .. }
                if node_id == "build" && skill == "nonexistent" && message == "Skill \"nonexistent\" was not found."
        )
    });
    assert!(found, "expected a missing_skill diagnostic for node build/nonexistent");
    assert_eq!(started.snapshot.nodes[0].state, DagNodeState::Pending);
}

#[test]
fn given_a_materialized_run_when_the_same_definition_is_submitted_again_then_the_fingerprint_ignores_the_skill_content_entirely()
 {
    let project = temp_dir();
    let cwd = temp_dir();
    write_skill(&cwd, "programming", "v1 skill body");
    let store = dag_store(&project);
    let nodes = definition(vec![node("build", "ship the feature", Some(vec!["programming"]))]);
    let with_skills = create_dag_manager(DagManagerOptions {
        store: std::sync::Arc::clone(&store),
        new_run_id: None,
        now: None,
        materialize_skills: Some(materializer(std::sync::Arc::clone(&store), &cwd)),
        settings: None,
    });
    let bare = create_dag_manager(DagManagerOptions {
        store: std::sync::Arc::clone(&store),
        new_run_id: None,
        now: None,
        materialize_skills: None,
        settings: None,
    });
    let materialized = with_skills
        .start(DagStartParams {
            definition: nodes.clone(),
            parent_session_id: PARENT_SESSION_ID.to_string(),
            root_session_id: ROOT_SESSION_ID.to_string(),
        })
        .expect("start with skills");

    let plain = bare
        .start(DagStartParams {
            definition: nodes,
            parent_session_id: "other-session".to_string(),
            root_session_id: ROOT_SESSION_ID.to_string(),
        })
        .expect("start bare");

    assert_eq!(materialized.snapshot.definition_fingerprint, plain.snapshot.definition_fingerprint);
}
