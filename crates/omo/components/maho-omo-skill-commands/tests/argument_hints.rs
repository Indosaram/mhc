//! Port of `omo-senpi/src/components/skill-commands/argument-hints.test.ts` at latest
//! `455dee623c9b2f2d2f1e9e68bf7b72a878ee3f0b`.
//!
//! senpi's slash picker waits for arguments only on a command that declares an argument hint; for a
//! skill that is the `argument-hint` frontmatter field. These tests read the SELECTED shipped skill
//! assets through the native frontmatter parser - never a synthetic fixture - so a shipped asset
//! regression fails here.

use std::path::{Path, PathBuf};

use maho_core::frontmatter::parse_frontmatter;
use maho_omo_skill_commands::read_bundled_skill_names;

const ARGUMENT_READING_SKILLS: [&str; 8] =
    ["adversarial-plan", "init-deep", "refactor", "remove-ai-slops", "ulw-execute", "ulw-loop", "ulw-plan", "ulw-research"];

/// The selected overlay is the packaging lane's deliverable. A checkout that has not staged it must
/// fail loudly rather than pass on a substitute.
const SELECTED_SKILLS_ENV: &str = "MAHO_OMO_LATEST_SKILLS_ROOT";

fn selected_assets_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../../assets/omo-latest")
}

fn skills_root() -> PathBuf {
    std::env::var(SELECTED_SKILLS_ENV).map(PathBuf::from).unwrap_or_else(|_| selected_assets_root().join("skills"))
}

fn require_skills_root() -> PathBuf {
    let root = skills_root();
    assert!(root.is_dir(), "the selected latest skill assets are required at {} (override with {SELECTED_SKILLS_ENV}); stage the packaging overlay first", root.display());
    root
}

fn argument_hint_of(path: &Path) -> Option<String> {
    let parsed = parse_frontmatter(&std::fs::read_to_string(path).expect("skill source")).expect("frontmatter");
    parsed.frontmatter.get("argument-hint").and_then(serde_json::Value::as_str).map(str::trim).filter(|hint| !hint.is_empty()).map(str::to_owned)
}

fn shipped_skill_files(root: &Path) -> Vec<(String, PathBuf)> {
    let mut files: Vec<(String, PathBuf)> = read_bundled_skill_names(Some(root))
        .expect("bundled skill names")
        .into_iter()
        .map(|name| (name.clone(), root.join(name).join("SKILL.md")))
        .collect();
    files.sort();
    files
}

#[test]
fn given_every_shipped_skill_when_its_frontmatter_is_parsed_then_exactly_the_argument_reading_skills_declare_a_hint() {
    let root = require_skills_root();
    let hinted: Vec<String> = shipped_skill_files(&root).into_iter().filter(|(_, path)| argument_hint_of(path).is_some()).map(|(name, _)| name).collect();

    assert_eq!(hinted, ARGUMENT_READING_SKILLS);
}

#[test]
fn given_a_shipped_skill_whose_body_consumes_the_typed_text_when_its_frontmatter_is_parsed_then_it_declares_a_hint() {
    let root = require_skills_root();
    let mut reading = Vec::new();
    let mut unhinted = Vec::new();
    for (name, path) in shipped_skill_files(&root) {
        let source = std::fs::read_to_string(&path).expect("skill source");
        if source.contains("$ARGUMENTS") || source.contains("<user-request>") {
            reading.push(name.clone());
            if argument_hint_of(&path).is_none() {
                unhinted.push(name);
            }
        }
    }

    assert!(!reading.is_empty(), "no argument-reading skill was found; the selected assets are the real shipped set");
    assert!(unhinted.is_empty(), "argument-reading skills must declare a hint: {unhinted:?}");
}

#[test]
fn given_the_selected_overlay_when_it_declares_the_ulw_skills_then_they_resolve_to_the_mit_lazycodex_copies() {
    let manifest_path = selected_assets_root().join("manifest.json");
    assert!(manifest_path.is_file(), "the selected overlay manifest is required at {}", manifest_path.display());
    let manifest: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&manifest_path).expect("manifest")).expect("manifest json");

    let skill_source = |name: &str| {
        manifest["skills"][name].as_array().and_then(|files| files.iter().find(|file| file["path"] == "SKILL.md")).and_then(|file| file["source"].as_str()).map(str::to_owned)
    };

    // The ulw skills ship from the MIT lazycodex copies, not from either SUL upstream pool.
    for name in ["ulw-execute", "ulw-plan"] {
        assert_eq!(skill_source(name), Some(format!("lazycodex@75b01747:plugins/omo/skills/{name}/SKILL.md (MIT)")), "{name}");
    }
    assert!(manifest["skills"].get("onboarding").is_some());
    assert!(manifest["skills"].get("mass-ulw").is_none(), "mass-ulw is retired");
}
