//! Quality gate artifact checks and compatibility.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use serde_json::Value;

use crate::domain_types::UlwLoopManualQaArtifactRef;
use crate::paths::is_within_attempt_dir;
use crate::quality_gate_fields::{
    invalid, is_poisoned, is_poisoned_artifact_kind, mark_poisoned_artifact_kind, section,
    string_array, text_field,
};

pub const SUPPORTED_SURFACES: &[&str] = &["cli", "http", "tmux", "browser", "gui", "data"];
pub const SUPPORTED_KINDS: &[&str] = &[
    "cli-transcript",
    "log",
    "screenshot",
    "image",
    "http-dump",
    "data-diff",
];

pub fn compatible_kinds_for(surface: &str) -> &'static [&'static str] {
    match surface {
        "cli" | "tmux" => &["cli-transcript", "log"],
        "http" => &["http-dump"],
        "browser" | "gui" => &["screenshot", "image"],
        "data" => &["data-diff"],
        _ => &[],
    }
}

pub fn artifact_compatible(surface: &str, kind: &str) -> bool {
    compatible_kinds_for(surface).contains(&kind)
}

pub fn surface_field(value: Option<&Value>, field: &str) -> String {
    let s = value.and_then(|v| v.as_str()).unwrap_or("");
    if SUPPORTED_SURFACES.contains(&s) {
        return s.to_string();
    }
    invalid(
        format!(
            "{field} must be a supported manual QA surface ({}).",
            SUPPORTED_SURFACES.join(", ")
        ),
        field,
    );
    "cli".to_string()
}

pub fn kind_field(value: Option<&Value>, field: &str) -> String {
    let s = value.and_then(|v| v.as_str()).unwrap_or("");
    if SUPPORTED_KINDS.contains(&s) {
        return s.to_string();
    }
    invalid(
        format!(
            "{field} must be a supported artifact kind ({}); review/QA reports belong in codeReview.reportPath or gateReview.reportPath, not artifactRefs.",
            SUPPORTED_KINDS.join(", ")
        ),
        field,
    );
    "log".to_string()
}

pub trait QualityGateFs {
    fn exists(&self, path: &Path) -> bool;
    fn size(&self, path: &Path) -> u64;
}

pub struct RealQualityGateFs;
impl QualityGateFs for RealQualityGateFs {
    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }
    fn size(&self, path: &Path) -> u64 {
        fs::metadata(path).map(|m| m.len()).unwrap_or(0)
    }
}

pub struct QualityGateCheckOptions<'a> {
    pub repo_root: Option<&'a Path>,
    pub fs: Option<&'a dyn QualityGateFs>,
    pub current_attempt_dir: Option<&'a str>,
}

pub fn check_file(path_str: &str, field: &str, opts: Option<&QualityGateCheckOptions>) {
    let Some(options) = opts else { return };
    let Some(root) = options.repo_root else { return };
    let fs = options.fs.unwrap_or(&RealQualityGateFs);
    if is_poisoned(field) {
        return;
    }

    let absolute = if Path::new(path_str).is_absolute() {
        PathBuf::from(path_str)
    } else {
        root.join(path_str)
    };

    if !fs.exists(&absolute) {
        invalid(format!("{field} must point to an existing artifact."), field);
        return;
    }
    if fs.size(&absolute) == 0 {
        invalid(format!("{field} must point to a non-empty artifact."), field);
    }
    if let Some(attempt_dir) = options.current_attempt_dir {
        let attempt_abs = root.join(attempt_dir);
        let abs_str = absolute.to_string_lossy();
        let att_str = attempt_abs.to_string_lossy();
        if !is_within_attempt_dir(&abs_str, &att_str) {
            invalid(
                format!("{field} ({path_str}) must point to an artifact from the current attempt ({attempt_dir})."),
                field,
            );
        }
    }
}

pub fn parse_artifact_refs(
    value: Option<&Value>,
    opts: Option<&QualityGateCheckOptions>,
) -> Vec<UlwLoopManualQaArtifactRef> {
    let Some(v) = value else {
        invalid("manualQa.artifactRefs must not be empty.", "manualQa.artifactRefs");
        return Vec::new();
    };
    let Some(arr) = v.as_array() else {
        invalid("manualQa.artifactRefs must not be empty.", "manualQa.artifactRefs");
        return Vec::new();
    };
    if arr.is_empty() {
        invalid("manualQa.artifactRefs must not be empty.", "manualQa.artifactRefs");
        return Vec::new();
    }

    let mut refs = Vec::new();
    for (index, item) in arr.iter().enumerate() {
        let prefix = format!("manualQa.artifactRefs[{index}]");
        let Some(ref_obj) = section(Some(item), &prefix) else {
            continue;
        };
        let path_field = format!("{prefix}.path");
        let id_field = format!("{prefix}.id");
        let kind_field_name = format!("{prefix}.kind");
        let desc_field = format!("{prefix}.description");

        let path = text_field(ref_obj.get("path"), &path_field);
        let id = text_field(ref_obj.get("id"), &id_field);
        let kind = kind_field(ref_obj.get("kind"), &kind_field_name);
        if is_poisoned(&kind_field_name) {
            mark_poisoned_artifact_kind(&id);
        }
        let description = text_field(ref_obj.get("description"), &desc_field);

        check_file(&path, &path_field, opts);

        refs.push(UlwLoopManualQaArtifactRef {
            id,
            kind,
            description,
            path,
        });
    }

    refs
}

pub fn artifact_map(
    refs: &[UlwLoopManualQaArtifactRef],
) -> BTreeMap<String, UlwLoopManualQaArtifactRef> {
    let mut by_id = BTreeMap::new();
    for r in refs {
        if by_id.contains_key(&r.id) {
            invalid(
                format!("manualQa.artifactRefs contains duplicate {}.", r.id),
                "manualQa.artifactRefs",
            );
        }
        by_id.insert(r.id.clone(), r.clone());
    }
    by_id
}

pub fn referenced_artifacts(
    value: Option<&Value>,
    field: &str,
    by_id: &BTreeMap<String, UlwLoopManualQaArtifactRef>,
) -> Vec<UlwLoopManualQaArtifactRef> {
    let ids = string_array(value, field);
    if is_poisoned(field) {
        return Vec::new();
    }
    let mut found = Vec::new();
    for id in ids {
        if is_poisoned_artifact_kind(&id) {
            continue;
        }
        if let Some(art) = by_id.get(&id) {
            found.push(art.clone());
        } else {
            invalid(format!("{field} references unknown artifact {id}."), field);
        }
    }
    found
}
