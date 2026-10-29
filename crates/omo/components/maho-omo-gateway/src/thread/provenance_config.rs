//! Sender provenance workspace configuration.
//!
//! Upstream counterpart: `thread/gateway/provenance-config.ts`. Only the sender's own runtime loads
//! this: a receiver renders only the provenance saved in the envelope.

use std::collections::BTreeMap;
use std::path::Path;

use omo_config_core::LoadOmoConfigOptions;
use maho_omo_config_resolution::load_senpi_omo_config;

use super::provenance_resolver::ProvenanceWorkspace;

/// The sender's `provenance.workspaces` from its profile-selected omo.json.
pub fn load_provenance_workspaces(cwd: &Path, env: &BTreeMap<String, String>) -> Vec<ProvenanceWorkspace> {
    let result = load_senpi_omo_config(LoadOmoConfigOptions {
        cwd: Some(cwd.to_string_lossy().into_owned()),
        env: Some(env.clone()),
        file_system: None,
        harness: None,
        platform: None,
        profile: None,
    });
    result
        .config
        .get("provenance")
        .and_then(|provenance| provenance.get("workspaces"))
        .and_then(|workspaces| serde_json::from_value::<Vec<ProvenanceWorkspace>>(workspaces.clone()).ok())
        .unwrap_or_default()
}
