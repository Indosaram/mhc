//! Port of senpi `packages/agent/src/conformance/mapping.ts`.

use std::collections::{BTreeMap, BTreeSet};

use super::registry::{AdapterKind, RegisteredCase};
use super::suite_hash::{SuiteHashEntry, compute_suite_hash, sha256_hex};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum MappingProblemCode {
    #[serde(rename = "duplicate-id")]
    DuplicateId,
    #[serde(rename = "malformed-id")]
    MalformedId,
    #[serde(rename = "malformed-impl")]
    MalformedImpl,
    #[serde(rename = "missing-intro")]
    MissingIntro,
    #[serde(rename = "missing-adapters")]
    MissingAdapters,
    #[serde(rename = "duplicate-adapter")]
    DuplicateAdapter,
    #[serde(rename = "missing-expected")]
    MissingExpected,
}

impl MappingProblemCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DuplicateId => "duplicate-id",
            Self::MalformedId => "malformed-id",
            Self::MalformedImpl => "malformed-impl",
            Self::MissingIntro => "missing-intro",
            Self::MissingAdapters => "missing-adapters",
            Self::DuplicateAdapter => "duplicate-adapter",
            Self::MissingExpected => "missing-expected",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MappingProblem {
    pub id: String,
    pub code: MappingProblemCode,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ManifestCase {
    pub id: String,
    #[serde(rename = "impl")]
    pub impl_path: String,
    pub intro: String,
    pub adapters: Vec<AdapterKind>,
    pub expected: String,
    pub impl_hash: String,
}

/// The exported suite: what every host runs and every host report is compared against.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ConformanceManifest {
    pub suite_hash: String,
    pub cases: Vec<ManifestCase>,
}

fn is_valid_case_id(id: &str) -> bool {
    let mut segments = id.split('-');
    let Some(first) = segments.next() else {
        return false;
    };
    if first.is_empty()
        || !first.bytes().next().is_some_and(|b| b.is_ascii_uppercase())
        || !first
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
    {
        return false;
    }
    let mut count = 0usize;
    for seg in segments {
        count += 1;
        if seg.is_empty()
            || !seg
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
        {
            return false;
        }
    }
    count >= 1
}

fn is_repo_relative_ts(path: &str) -> bool {
    path.ends_with(".ts")
        && !path.starts_with('/')
        && !path.contains('\\')
        && !path.split('/').any(|seg| seg == "..")
}

fn problems_of(entry: &RegisteredCase) -> Vec<MappingProblemCode> {
    let mut codes = Vec::new();
    if !is_valid_case_id(entry.id) {
        codes.push(MappingProblemCode::MalformedId);
    }
    if !is_repo_relative_ts(entry.impl_path) {
        codes.push(MappingProblemCode::MalformedImpl);
    }
    if entry.intro.trim().is_empty() {
        codes.push(MappingProblemCode::MissingIntro);
    }
    if entry.adapters.is_empty() {
        codes.push(MappingProblemCode::MissingAdapters);
    }
    let unique_adapters: BTreeSet<AdapterKind> = entry.adapters.iter().copied().collect();
    if unique_adapters.len() != entry.adapters.len() {
        codes.push(MappingProblemCode::DuplicateAdapter);
    }
    if entry.expected.trim().is_empty() {
        codes.push(MappingProblemCode::MissingExpected);
    }
    codes
}

/// Every reason the registry is not a complete, unambiguous mapping; empty when it is.
pub fn find_mapping_problems(cases: &[RegisteredCase]) -> Vec<MappingProblem> {
    let mut problems = Vec::new();
    let mut seen = BTreeSet::new();
    for entry in cases {
        if !seen.insert(entry.id) {
            problems.push(MappingProblem {
                id: entry.id.to_string(),
                code: MappingProblemCode::DuplicateId,
            });
        }
        for code in problems_of(entry) {
            problems.push(MappingProblem {
                id: entry.id.to_string(),
                code,
            });
        }
    }
    problems
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImplementationGaps {
    pub unimplemented: Vec<String>,
    pub unregistered: Vec<String>,
}

/// Registered cases with no implementation, and implementations with no registered case.
pub fn find_implementation_gaps<V>(
    cases: &[RegisteredCase],
    implementations: &BTreeMap<&'static str, V>,
) -> ImplementationGaps {
    let registered: BTreeSet<&'static str> = cases.iter().map(|entry| entry.id).collect();
    let unimplemented = registered
        .iter()
        .filter(|id| !implementations.contains_key(*id))
        .map(|id| (*id).to_string())
        .collect();
    let unregistered = implementations
        .keys()
        .filter(|id| !registered.contains(*id))
        .map(|id| (*id).to_string())
        .collect();
    ImplementationGaps {
        unimplemented,
        unregistered,
    }
}

/// Build the manifest export, hashing each implementation file through `read_impl` (repository-relative path).
pub async fn build_manifest<F, Fut>(
    cases: &[RegisteredCase],
    mut read_impl: F,
) -> Result<ConformanceManifest, String>
where
    F: FnMut(&str) -> Fut,
    Fut: std::future::Future<Output = Result<Vec<u8>, String>>,
{
    let problems = find_mapping_problems(cases);
    if !problems.is_empty() {
        let joined: Vec<String> = problems
            .iter()
            .map(|p| format!("{} {}", p.id, p.code.as_str()))
            .collect();
        return Err(format!(
            "conformance registry mapping problems: {}",
            joined.join(", ")
        ));
    }
    let mut manifest_cases = Vec::with_capacity(cases.len());
    let mut hash_entries = Vec::with_capacity(cases.len());
    for entry in cases {
        let bytes = read_impl(entry.impl_path).await.map_err(|err| {
            format!(
                "conformance case {}: cannot read its implementation {}: {err}",
                entry.id, entry.impl_path
            )
        })?;
        let impl_hash = sha256_hex(&bytes);
        hash_entries.push(SuiteHashEntry {
            id: entry.id.to_string(),
            impl_hash: impl_hash.clone(),
        });
        manifest_cases.push(ManifestCase {
            id: entry.id.to_string(),
            impl_path: entry.impl_path.to_string(),
            intro: entry.intro.to_string(),
            adapters: entry.adapters.to_vec(),
            expected: entry.expected.to_string(),
            impl_hash,
        });
    }
    let suite_hash = compute_suite_hash(&hash_entries)?;
    Ok(ConformanceManifest {
        suite_hash,
        cases: manifest_cases,
    })
}
