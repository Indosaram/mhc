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

const ARGUMENT_READING_SKILLS: [&str; 9] =
    ["hyperplan", "init-deep", "mass-ulw", "refactor", "remove-ai-slops", "ulw-execute", "ulw-loop", "ulw-plan", "ulw-research"];

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
fn given_a_name_staged_from_both_pools_when_the_selected_overlay_resolves_it_then_the_senpi_native_copy_wins() {
    let manifest_path = selected_assets_root().join("manifest.json");
    assert!(manifest_path.is_file(), "the selected overlay manifest is required at {}", manifest_path.display());
    let manifest: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&manifest_path).expect("manifest")).expect("manifest json");

    let skill_source = |name: &str| {
        manifest["skills"][name].as_array().and_then(|files| files.iter().find(|file| file["path"] == "SKILL.md")).and_then(|file| file["source"].as_str()).map(str::to_owned)
    };

    // ulw-execute ships only from the shared pool; ulw-plan is shadowed by the senpi-native copy.
    assert_eq!(skill_source("ulw-execute").as_deref(), Some("packages/shared-skills/skills/ulw-execute/SKILL.md"));
    assert_eq!(skill_source("ulw-plan").as_deref(), Some("packages/omo-senpi/skills/ulw-plan/SKILL.md"));
    assert!(manifest["skills"].get("onboarding").is_some());
}
