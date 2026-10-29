//! The TUI's shared scan (upstream `tui-scan-runtime.ts`): `omo setup`'s detection, plus the engine session
//! history when `sessions` is true, so the TUI never grows a second reader. Its digests stay in memory with
//! the calling process's cached scan, like the RPC scan's: nothing is on disk.

use std::collections::{BTreeMap, HashSet};

use crate::scan_contract::OnboardingScanEvent;
use crate::scan_runtime::{
    OnboardingScanEnvironment, OnboardingScanOptions, migration_facts_from_scan,
    onboarding_scan_context, run_onboarding_scan,
};
use crate::session_history::SessionDigest;

#[derive(Clone, Debug)]
pub struct TuiScanResult {
    pub scan_id: String,
    /// The scan's session digests, held in memory only.
    pub digests: Vec<SessionDigest>,
    /// Project roots the scan listed; understanding reads AGENTS.md only inside these.
    pub project_roots: HashSet<String>,
    pub env: Option<BTreeMap<String, String>>,
    /// Scan rows and value summary, computed from this scan's events.
    pub context: String,
    pub inventory: Option<String>,
}

pub fn run_tui_onboarding_scan(
    environment: &OnboardingScanEnvironment,
    sessions: bool,
) -> TuiScanResult {
    let scan_id = uuid::Uuid::new_v4().to_string();
    let mut events: Vec<OnboardingScanEvent> = Vec::new();
    let outcome = run_onboarding_scan(
        environment,
        &OnboardingScanOptions {
            scan_id: scan_id.clone(),
            include_custom_providers: true,
            sessions,
        },
        &mut |event| events.push(event),
    );
    let inventory = migration_facts_from_scan(&outcome.results);
    TuiScanResult {
        context: onboarding_scan_context(&scan_id, &events),
        scan_id,
        digests: outcome.digests,
        project_roots: outcome.project_roots,
        env: environment.env.clone(),
        inventory,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_setup_only_scan_has_a_context_and_no_digests() {
        let environment = OnboardingScanEnvironment {
            home: Some("/nonexistent-home".into()),
            env: Some(BTreeMap::new()),
            ..Default::default()
        };
        let result = run_tui_onboarding_scan(&environment, false);
        assert!(result.digests.is_empty() && result.inventory.is_none());
        assert!(
            result
                .context
                .starts_with(&format!("Onboarding scan {} ", result.scan_id))
        );
        assert!(
            result
                .context
                .ends_with("Value summary: nothing found to carry over.")
        );
    }
}
