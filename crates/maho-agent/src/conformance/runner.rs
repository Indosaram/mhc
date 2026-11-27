//! Port of senpi `packages/agent/src/conformance/runner.ts`.

use std::collections::BTreeMap;
use std::sync::Arc;

use maho_ai::types::BoxFuture;

use super::mapping::ConformanceManifest;
use super::registry::AdapterKind;

pub type ConformanceResult = Result<(), String>;

/// A case passes when it returns `Ok(())` and fails when it returns `Err(message)`.
pub type CaseImplementation<A> =
    Arc<dyn Fn(A) -> BoxFuture<'static, ConformanceResult> + Send + Sync>;

/// A host under test: its name in reports, the adapter kinds it provides, and the adapters handed to each case.
#[derive(Clone)]
pub struct ConformanceHost<TAdapters> {
    pub name: String,
    pub provides: Vec<AdapterKind>,
    pub adapters: TAdapters,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "status")]
pub enum CaseResult {
    #[serde(rename = "pass")]
    Pass { id: String },
    #[serde(rename = "fail")]
    Fail { id: String, error: String },
}

/// One host's run of the manifest. There is no skip: a case that cannot run on the host fails.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HostReport {
    pub host: String,
    pub suite_hash: String,
    pub results: Vec<CaseResult>,
}

async fn run_case<TAdapters: Clone>(
    id: &str,
    adapters: &[AdapterKind],
    host: &ConformanceHost<TAdapters>,
    implementation: Option<&CaseImplementation<TAdapters>>,
) -> CaseResult {
    let Some(implementation) = implementation else {
        return CaseResult::Fail {
            id: id.to_string(),
            error: "no implementation is registered for this case".to_string(),
        };
    };
    let missing: Vec<&'static str> = adapters
        .iter()
        .filter(|kind| !host.provides.contains(kind))
        .map(|kind| kind.as_str())
        .collect();
    if !missing.is_empty() {
        return CaseResult::Fail {
            id: id.to_string(),
            error: format!("the host does not provide adapter: {}", missing.join(", ")),
        };
    }
    match implementation(host.adapters.clone()).await {
        Ok(()) => CaseResult::Pass { id: id.to_string() },
        Err(error) => CaseResult::Fail {
            id: id.to_string(),
            error,
        },
    }
}

/// Run every manifest case on `host`, one at a time in manifest order, and report under the manifest's suite hash.
pub async fn run_suite<TAdapters: Clone>(
    manifest: &ConformanceManifest,
    host: &ConformanceHost<TAdapters>,
    implementations: &BTreeMap<&'static str, CaseImplementation<TAdapters>>,
) -> HostReport {
    let mut results = Vec::with_capacity(manifest.cases.len());
    for entry in &manifest.cases {
        results.push(
            run_case(
                &entry.id,
                &entry.adapters,
                host,
                implementations.get(entry.id.as_str()),
            )
            .await,
        );
    }
    HostReport {
        host: host.name.clone(),
        suite_hash: manifest.suite_hash.clone(),
        results,
    }
}
