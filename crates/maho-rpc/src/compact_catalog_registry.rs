//! senpi `modes/rpc/compact-catalog-registry.ts`: the versioned checkpoint schema's families,
//! entry-kind mapping, producer completeness tracking, and `families/families` decoder.

use crate::accepted_transcript::TranscriptRoot;
use crate::compact_catalog_codec::{BoundedId, protocol_id};
use crate::compact_read_lease::CompactError;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CatalogFamily {
    Transcript,
    Run,
    Lifecycle,
    Dialogs,
    Opaque,
    Active,
    Calls,
    Settled,
    Identity,
    Source,
}

impl CatalogFamily {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Transcript => "transcript",
            Self::Run => "run",
            Self::Lifecycle => "lifecycle",
            Self::Dialogs => "dialogs",
            Self::Opaque => "opaque",
            Self::Active => "active",
            Self::Calls => "calls",
            Self::Settled => "settled",
            Self::Identity => "identity",
            Self::Source => "source",
        }
    }
}

pub const CATALOG_FAMILIES: [CatalogFamily; 10] = [
    CatalogFamily::Transcript,
    CatalogFamily::Run,
    CatalogFamily::Lifecycle,
    CatalogFamily::Dialogs,
    CatalogFamily::Opaque,
    CatalogFamily::Active,
    CatalogFamily::Calls,
    CatalogFamily::Settled,
    CatalogFamily::Identity,
    CatalogFamily::Source,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FamilyProducer {
    Complete,
    Partial,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FamilyProducers {
    pub transcript: FamilyProducer,
    pub run: FamilyProducer,
    pub lifecycle: FamilyProducer,
    pub dialogs: FamilyProducer,
    pub opaque: FamilyProducer,
    pub active: FamilyProducer,
    pub calls: FamilyProducer,
    pub settled: FamilyProducer,
    pub identity: FamilyProducer,
    pub source: FamilyProducer,
}

impl FamilyProducers {
    pub fn get(&self, family: CatalogFamily) -> FamilyProducer {
        match family {
            CatalogFamily::Transcript => self.transcript,
            CatalogFamily::Run => self.run,
            CatalogFamily::Lifecycle => self.lifecycle,
            CatalogFamily::Dialogs => self.dialogs,
            CatalogFamily::Opaque => self.opaque,
            CatalogFamily::Active => self.active,
            CatalogFamily::Calls => self.calls,
            CatalogFamily::Settled => self.settled,
            CatalogFamily::Identity => self.identity,
            CatalogFamily::Source => self.source,
        }
    }

    pub fn set(&mut self, family: CatalogFamily, producer: FamilyProducer) {
        match family {
            CatalogFamily::Transcript => self.transcript = producer,
            CatalogFamily::Run => self.run = producer,
            CatalogFamily::Lifecycle => self.lifecycle = producer,
            CatalogFamily::Dialogs => self.dialogs = producer,
            CatalogFamily::Opaque => self.opaque = producer,
            CatalogFamily::Active => self.active = producer,
            CatalogFamily::Calls => self.calls = producer,
            CatalogFamily::Settled => self.settled = producer,
            CatalogFamily::Identity => self.identity = producer,
            CatalogFamily::Source => self.source = producer,
        }
    }
}

pub const FAMILY_PRODUCERS: FamilyProducers = FamilyProducers {
    transcript: FamilyProducer::Complete,
    run: FamilyProducer::Complete,
    lifecycle: FamilyProducer::Complete,
    dialogs: FamilyProducer::Complete,
    opaque: FamilyProducer::Complete,
    active: FamilyProducer::Complete,
    calls: FamilyProducer::Complete,
    settled: FamilyProducer::Complete,
    identity: FamilyProducer::Complete,
    source: FamilyProducer::Complete,
};

pub fn family_producers(
    transcript: &TranscriptRoot,
    refused_correlations: usize,
) -> FamilyProducers {
    let mut producers = FAMILY_PRODUCERS;
    if transcript.unavailable.is_some() {
        producers.transcript = FamilyProducer::Partial;
        producers.opaque = FamilyProducer::Partial;
        producers.source = FamilyProducer::Partial;
    }
    if refused_correlations > 0 {
        producers.identity = FamilyProducer::Partial;
    }
    producers
}

pub const ENTRY_FAMILIES: [(&str, CatalogFamily); 14] = [
    ("session", CatalogFamily::Transcript),
    ("message", CatalogFamily::Transcript),
    ("thinking_level_change", CatalogFamily::Source),
    ("configuration_update", CatalogFamily::Source),
    ("model_change", CatalogFamily::Source),
    ("model_change_rejected", CatalogFamily::Source),
    ("usage", CatalogFamily::Source),
    ("compaction", CatalogFamily::Source),
    ("branch_summary", CatalogFamily::Source),
    ("custom", CatalogFamily::Source),
    ("custom_message", CatalogFamily::Source),
    ("context_edit", CatalogFamily::Source),
    ("label", CatalogFamily::Source),
    ("session_info", CatalogFamily::Source),
];

pub fn entry_family(kind: &str) -> CatalogFamily {
    ENTRY_FAMILIES
        .iter()
        .find(|(known, _)| *known == kind)
        .map_or(CatalogFamily::Opaque, |(_, family)| *family)
}

pub fn family_id(family: CatalogFamily) -> BoundedId {
    protocol_id(family.as_str())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemberPrefix {
    Entry,
    Dialog,
    Message,
    Call,
    Join,
}

impl MemberPrefix {
    fn as_str(self) -> &'static str {
        match self {
            Self::Entry => "entry",
            Self::Dialog => "dialog",
            Self::Message => "message",
            Self::Call => "call",
            Self::Join => "join",
        }
    }
}

pub enum MemberOrdinal<'a> {
    Number(u64),
    Text(&'a str),
}

pub fn member_id(prefix: MemberPrefix, ordinal: MemberOrdinal<'_>) -> BoundedId {
    match ordinal {
        MemberOrdinal::Number(n) => protocol_id(&format!("{}-{n:016}", prefix.as_str())),
        MemberOrdinal::Text(s) => protocol_id(&format!("{}-{s}", prefix.as_str())),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FamilyRow {
    pub family: CatalogFamily,
    pub producer: FamilyProducer,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompleteFamilyRegistry {
    pub format: String,
    pub version: u64,
    pub families: Vec<FamilyRow>,
    pub required_kinds: Vec<CatalogFamily>,
    pub assembly: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IncompleteFamilyRegistry {
    pub format: String,
    pub version: u64,
    pub families: Vec<FamilyRow>,
    pub required_kinds: Vec<CatalogFamily>,
    pub assembly: String,
    pub incomplete: Vec<CatalogFamily>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum FamilyRegistry {
    Complete(CompleteFamilyRegistry),
    Incomplete(IncompleteFamilyRegistry),
}

impl FamilyRegistry {
    pub fn families(&self) -> &[FamilyRow] {
        match self {
            Self::Complete(reg) => &reg.families,
            Self::Incomplete(reg) => &reg.families,
        }
    }

    pub fn required_kinds(&self) -> &[CatalogFamily] {
        match self {
            Self::Complete(reg) => &reg.required_kinds,
            Self::Incomplete(reg) => &reg.required_kinds,
        }
    }

    pub fn is_complete(&self) -> bool {
        matches!(self, Self::Complete(_))
    }
}

pub fn family_registry(producers: &FamilyProducers) -> FamilyRegistry {
    let families: Vec<FamilyRow> = CATALOG_FAMILIES
        .iter()
        .map(|&family| FamilyRow {
            family,
            producer: producers.get(family),
        })
        .collect();
    let incomplete: Vec<CatalogFamily> = families
        .iter()
        .filter(|row| row.producer != FamilyProducer::Complete)
        .map(|row| row.family)
        .collect();
    let mut required_kinds: Vec<CatalogFamily> = families
        .iter()
        .filter(|row| row.producer == FamilyProducer::Complete)
        .map(|row| row.family)
        .collect();
    required_kinds.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    if incomplete.is_empty() {
        FamilyRegistry::Complete(CompleteFamilyRegistry {
            format: "senpi-recovery-families".into(),
            version: 1,
            families,
            required_kinds,
            assembly: "complete".into(),
        })
    } else {
        FamilyRegistry::Incomplete(IncompleteFamilyRegistry {
            format: "senpi-recovery-families".into(),
            version: 1,
            families,
            required_kinds,
            assembly: "incomplete".into(),
            incomplete,
        })
    }
}

pub fn complete_assembly(registry: &FamilyRegistry) -> Option<&CompleteFamilyRegistry> {
    match registry {
        FamilyRegistry::Complete(complete) => Some(complete),
        FamilyRegistry::Incomplete(_) => None,
    }
}

/// The completeness gate (X03): every family whose producer is `complete` must have emitted at
/// least one component, an explicit empty included.
pub fn assert_coverage(
    registry: &FamilyRegistry,
    emitted: &HashSet<BoundedId>,
) -> Result<(), CompactError> {
    let missing = registry.families().iter().any(|row| {
        row.producer == FamilyProducer::Complete && !emitted.contains(&family_id(row.family))
    });
    if missing {
        return Err(CompactError::Io(
            "compact read refused (unavailable)".into(),
        ));
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawFamilyRegistry {
    format: String,
    version: u64,
    families: Vec<RawFamilyRow>,
    assembly: String,
    required_kinds: Vec<String>,
}

#[derive(Deserialize)]
struct RawFamilyRow {
    family: String,
    producer: String,
}

fn parse_family(s: &str) -> Option<CatalogFamily> {
    CATALOG_FAMILIES.iter().copied().find(|f| f.as_str() == s)
}

fn parse_producer(s: &str) -> Option<FamilyProducer> {
    match s {
        "complete" => Some(FamilyProducer::Complete),
        "partial" => Some(FamilyProducer::Partial),
        _ => None,
    }
}

/// The one decoder of a `families/families` component (RULING-61 Q3).
pub fn read_family_registry(text: &str) -> Result<FamilyRegistry, CompactError> {
    let raw: RawFamilyRegistry =
        serde_json::from_str(text).map_err(|_| CompactError::Io("not a family registry".into()))?;
    if raw.format != "senpi-recovery-families"
        || raw.version != 1
        || raw.families.len() != CATALOG_FAMILIES.len()
    {
        return Err(CompactError::Io("not a family registry".into()));
    }
    let mut producers = FAMILY_PRODUCERS;
    let mut seen = BTreeMap::new();
    for (index, row) in raw.families.iter().enumerate() {
        let Some(family) = parse_family(&row.family) else {
            return Err(CompactError::Io("not a family registry".into()));
        };
        if family != CATALOG_FAMILIES[index] {
            return Err(CompactError::Io("not a family registry".into()));
        }
        let Some(producer) = parse_producer(&row.producer) else {
            return Err(CompactError::Io("not a family registry".into()));
        };
        seen.insert(family, producer);
        producers.set(family, producer);
    }
    let registry = family_registry(&producers);
    let expected_assembly = if registry.is_complete() {
        "complete"
    } else {
        "incomplete"
    };
    if raw.assembly != expected_assembly {
        return Err(CompactError::Io("family registry assembly mismatch".into()));
    }
    let expected_required: Vec<&str> = registry
        .required_kinds()
        .iter()
        .map(|f| f.as_str())
        .collect();
    let actual_required: Vec<&str> = raw.required_kinds.iter().map(String::as_str).collect();
    if actual_required != expected_required {
        return Err(CompactError::Io(
            "family registry requiredKinds mismatch".into(),
        ));
    }
    Ok(registry)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrips_complete_and_incomplete_family_registries() {
        let full = family_registry(&FAMILY_PRODUCERS);
        assert!(complete_assembly(&full).is_some());
        let json = serde_json::to_string(&full).expect("serialize");
        let decoded = read_family_registry(&json).expect("decode");
        assert_eq!(decoded, full);

        let mut partial_producers = FAMILY_PRODUCERS;
        partial_producers.transcript = FamilyProducer::Partial;
        partial_producers.opaque = FamilyProducer::Partial;
        partial_producers.source = FamilyProducer::Partial;
        let partial = family_registry(&partial_producers);
        assert!(complete_assembly(&partial).is_none());
        let json_partial = serde_json::to_string(&partial).expect("serialize partial");
        assert_eq!(
            read_family_registry(&json_partial).expect("decode partial"),
            partial
        );

        let mut tampered: serde_json::Value = serde_json::from_str(&json_partial).expect("json");
        tampered["assembly"] = serde_json::json!("complete");
        assert!(read_family_registry(&tampered.to_string()).is_err());
    }

    #[test]
    fn coverage_gate_refuses_missing_complete_family() {
        let full = family_registry(&FAMILY_PRODUCERS);
        let mut emitted: HashSet<BoundedId> =
            CATALOG_FAMILIES.iter().map(|&f| family_id(f)).collect();
        assert!(assert_coverage(&full, &emitted).is_ok());
        emitted.remove(&family_id(CatalogFamily::Calls));
        assert!(assert_coverage(&full, &emitted).is_err());
    }
}
