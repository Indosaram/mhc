//! `dag/skills.ts`: once-at-creation `load_skills` materialization into `effectivePrompt`.
// allow: SIZE_OK - materialization, manifest persistence, and resume readback share one contract.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::dag::graph::DagNodeInput;
use crate::dag::manager::DagMaterializeSkills;
use crate::dag::store::DagFileStore;
use crate::dag::types::{DagDiagnostic, DagNodeId, DagRunId};
use crate::tools::task::skills::{build_skill_prepend, create_fs_skill_loader, FsSkillLoaderOptions};
use crate::tools::task::types::{LoadedSkill, SkillLoader};

const SCHEMA_VERSION: u8 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DagSkillDigest {
    pub name: String,
    pub sha256: String,
}

/// Everything the dispatcher and an auditor need about one node's skills, captured once at run
/// creation. `prompt` is the submitted original; `effective_prompt` is the dispatch material.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DagNodeSkillMaterialization {
    pub node_id: String,
    pub requested: Vec<String>,
    pub resolved: Vec<DagSkillDigestWire>,
    pub missing: Vec<String>,
    pub prompt: String,
    pub effective_prompt: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DagSkillDigestWire {
    pub name: String,
    pub sha256: String,
}

/// Persisted sidecar for a run. `cwd` is the PINNED run-creation working directory: dispatch and
/// resume read this manifest instead of re-searching the filesystem, so a later cwd change or a
/// SKILL.md edit can never alter an existing run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DagSkillManifest {
    pub schema_version: u8,
    pub run_id: DagRunId,
    pub cwd: String,
    pub at: String,
    pub nodes: Vec<DagNodeSkillMaterialization>,
}

pub struct DagSkillMaterializerOptions {
    pub store: Arc<DagFileStore>,
    pub cwd: String,
    pub load_skills: Option<Arc<SkillLoader>>,
    pub home_dir: Option<PathBuf>,
    pub extra_dirs: Vec<PathBuf>,
}

fn manifest_path(store: &DagFileStore, run_id: &str) -> PathBuf {
    store.paths.root.join("skills").join(format!("{run_id}.json"))
}

fn sha256_hex(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Third-party loaders may still expose only the v1 ready-to-prepend block. Native filesystem
/// loaders also expose the parsed content + location so DAG materialization preserves the exact
/// invocation wrapper used by direct task spawns.
fn skill_content(load_skills: &SkillLoader, name: &str, cwd: &str) -> Option<LoadedSkill> {
    let resolution = load_skills(std::slice::from_ref(&name.to_string()), cwd);
    if resolution.resolved.is_empty() {
        return None;
    }
    if let Some(loaded) = resolution
        .skills
        .as_ref()
        .and_then(|skills| skills.iter().find(|skill| skill.name == name))
    {
        return Some(loaded.clone());
    }
    let prefix = format!("<skill name=\"{name}\">\n");
    let suffix = "\n</skill>\n\n";
    let block = resolution.prepend;
    if !block.starts_with(&prefix) || !block.ends_with(suffix) {
        return None;
    }
    let content = &block[prefix.len()..block.len() - suffix.len()];
    Some(LoadedSkill {
        name: name.to_string(),
        content: content.to_string(),
        location: None,
    })
}

fn materialize_node(
    node: &DagNodeInput,
    load_skills: &SkillLoader,
    cwd: &str,
) -> (DagNodeSkillMaterialization, Vec<String>) {
    let requested = node.load_skills.clone().unwrap_or_default();
    let mut contents: Vec<LoadedSkill> = Vec::new();
    let mut missing: Vec<String> = Vec::new();
    for name in &requested {
        match skill_content(load_skills, name, cwd) {
            Some(content) => contents.push(content),
            None => missing.push(name.clone()),
        }
    }
    let resolved: Vec<DagSkillDigestWire> = contents
        .iter()
        .map(|skill| DagSkillDigestWire {
            name: skill.name.clone(),
            sha256: sha256_hex(&skill.content),
        })
        .collect();
    let effective_prompt = build_skill_prepend(&contents, &node.prompt);
    (
        DagNodeSkillMaterialization {
            node_id: node.id.clone(),
            requested,
            resolved,
            missing: missing.clone(),
            prompt: node.prompt.clone(),
            effective_prompt,
        },
        missing,
    )
}

/// Resolves every node's `load_skills` ONCE, at run creation, against a pinned cwd.
///
/// Missing skills never fail the run: each one becomes a `missing_skill` node diagnostic and the
/// node keeps dispatching with whatever resolved. The resulting effectivePrompt and the digests
/// are DISPATCH MATERIAL and audit metadata only - the run fingerprint covers the submitted
/// definition alone, so nothing here may ever reach it.
pub fn create_dag_skill_materializer(options: DagSkillMaterializerOptions) -> DagMaterializeSkills {
    let store = options.store;
    let cwd = options.cwd;
    let load_skills = options.load_skills.unwrap_or_else(|| {
        create_fs_skill_loader(FsSkillLoaderOptions {
            home_dir: options.home_dir,
            extra_dirs: options.extra_dirs,
            ..Default::default()
        })
    });
    Arc::new(move |input| {
        let mut nodes: Vec<DagNodeSkillMaterialization> = Vec::new();
        let mut diagnostics: Vec<DagDiagnostic> = Vec::new();
        for node in &input.definition.nodes {
            let (materialized, missing) = materialize_node(node, load_skills.as_ref(), &cwd);
            for name in missing {
                diagnostics.push(DagDiagnostic::MissingSkill {
                    node_id: node.id.clone() as DagNodeId,
                    skill: name.clone(),
                    message: format!("Skill \"{name}\" was not found."),
                    at: input.at.to_string(),
                });
            }
            nodes.push(materialized);
        }
        let manifest = DagSkillManifest {
            schema_version: SCHEMA_VERSION,
            run_id: input.run_id.clone(),
            cwd: cwd.clone(),
            at: input.at.to_string(),
            nodes: nodes.clone(),
        };
        let path = manifest_path(&store, input.run_id);
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        // The manifest shape is a fixed, always-serializable struct.
        let serialized = serde_json::to_string(&manifest).unwrap_or_default();
        let _ = fs::write(&path, serialized);
        crate::dag::manager::DagSkillMaterialization {
            nodes: nodes
                .into_iter()
                .map(|node| (node.node_id, node.effective_prompt))
                .collect(),
            diagnostics,
        }
    })
}

/// Resume path: read the creation-time materialization, never `SKILL.md`.
pub fn read_dag_skill_manifest(store: &DagFileStore, run_id: &str) -> Option<DagSkillManifest> {
    let raw = fs::read_to_string(manifest_path(store, run_id)).ok()?;
    serde_json::from_str(&raw).ok()
}

#[cfg(test)]
#[path = "skills_tests.rs"]
mod tests;
