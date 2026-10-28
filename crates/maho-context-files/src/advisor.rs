//! The session-start suggestion to create context files (spec §6).

use crate::analyze::{Analysis, CONTEXT_FILES};
use crate::plan::{Settings, choose};
use std::path::{Path, PathBuf};

/// Entries the advisor walks at most (session start must stay fast in very large trees).
pub const ADVICE_WALK_LIMIT: usize = 50_000;

/// `<home>/.omo/state/context-advice.json`.
pub fn state_file(home: &Path) -> PathBuf {
    home.join(".omo").join("state").join("context-advice.json")
}

fn advised(state: &Path) -> Vec<String> {
    std::fs::read_to_string(state)
        .ok()
        .and_then(|text| serde_json::from_str::<Vec<String>>(&text).ok())
        .unwrap_or_default()
}

/// Whether `root` was advised before.
pub fn was_advised(state: &Path, root: &Path) -> bool {
    let root = root.to_string_lossy();
    advised(state).iter().any(|r| *r == root)
}

/// Records `root` as advised.
pub fn record(state: &Path, root: &Path) -> std::io::Result<()> {
    let mut roots = advised(state);
    let root = root.to_string_lossy().into_owned();
    if !roots.contains(&root) {
        roots.push(root);
    }
    if let Some(dir) = state.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let temp = state.with_extension(format!("tmp-{}", std::process::id()));
    std::fs::write(
        &temp,
        serde_json::to_string_pretty(&roots).unwrap_or_default(),
    )?;
    std::fs::rename(&temp, state)
}

/// The suggestion for `analysis`, or `None` when there is nothing to suggest.
pub fn advice(analysis: &Analysis, settings: &Settings) -> Option<String> {
    if !settings.advise {
        return None;
    }
    if CONTEXT_FILES
        .iter()
        .any(|name| analysis.root.join(name).is_file())
    {
        return None;
    }
    let root = analysis.dirs.get(".")?;
    if root.subtree_code_files < settings.min_files {
        return None;
    }
    let count = choose(analysis, settings).len();
    Some(format!(
        "This repository has no AGENTS.md. /init-deep can write context files for {count} director{} so agents start with the project's layout and commands.",
        if count == 1 { "y" } else { "ies" }
    ))
}

/// The advice for `root` once ever: checks and records the state file.
pub fn advise_once(home: &Path, root: &Path, settings: &Settings) -> Option<String> {
    if !settings.advise || !root.join(".git").exists() {
        return None;
    }
    let state = state_file(home);
    if was_advised(&state, root) {
        return None;
    }
    let analysis = crate::analyze::analyze_bounded(root, ADVICE_WALK_LIMIT);
    let text = advice(&analysis, settings)?;
    if let Err(error) = record(&state, root) {
        maho_ext_api::diagnostic(&format!("context files: cannot record advice: {error}"));
    }
    Some(text)
}
