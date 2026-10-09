use super::file_location::PostEditFileLocation;
use std::collections::HashSet;
use std::sync::{Mutex, PoisonError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostEditNotInstalledOutcome {
    pub server_id: String,
    pub install_decision_tool: bool,
    pub decision: Option<String>,
    pub text: String,
}

pub fn not_installed_guidance(
    file_path: &str,
    outcome: &PostEditNotInstalledOutcome,
    state: &Mutex<HashSet<String>>,
    locate_file: Option<&(dyn Fn(&str) -> PostEditFileLocation + Sync)>,
) -> Option<String> {
    if outcome.decision.is_some() {
        return Some(outcome.text.clone());
    }
    if locate_file.is_some_and(|locate| locate(file_path) != PostEditFileLocation::Project) {
        return None;
    }
    if outcome.install_decision_tool {
        return Some(outcome.text.clone());
    }
    if !state.lock().unwrap_or_else(PoisonError::into_inner).insert(outcome.server_id.clone()) {
        return None;
    }
    Some(outcome.text.clone())
}
