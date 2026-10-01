//! Prints the skill-loading diagnostics the Rust port produces for a directory, as JSON.
//!
//! The golden harness (tools/golden/system-prompt.mjs) runs this and senpi's loadSkills over the same
//! fixture and compares the diagnostics' type, path and skip decision.
//!
//! Usage: skill_diagnostics_fixture <dir>

use maho_core::skills::{LoadSkillsFromDirOptions, load_skills_from_dir};

fn main() {
    let dir = std::env::args().nth(1).unwrap_or_else(|| ".".to_string());
    let result = load_skills_from_dir(&LoadSkillsFromDirOptions { dir: dir.clone(), source: "path".to_string() });
    let diagnostics: Vec<serde_json::Value> = result
        .diagnostics
        .iter()
        .map(|diagnostic| {
            serde_json::json!({
                "type": format!("{:?}", diagnostic.diagnostic_type).to_lowercase(),
                "path": diagnostic.path,
                "message": diagnostic.message,
            })
        })
        .collect();
    let output = serde_json::json!({
        "skills": result.skills.iter().map(|skill| skill.name.clone()).collect::<Vec<_>>(),
        "diagnostics": diagnostics,
    });
    println!("{}", serde_json::to_string_pretty(&output).unwrap_or_default());
}
