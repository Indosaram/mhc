//! senpi `modes/rpc/compact-recovery-manifest-shape.ts`: the compact-v1 manifest (R4.B1) and
//! the catalog family registry descriptors shared with `compact_recovery_manifest_kinds` and
//! `compact_recovery_manifest`.

use crate::compact_recovery_ids::{BoundedId, ContentDigest, ContentRef, protocol_id};
use crate::compact_recovery_types::RecoveryRouteKind;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The versioned checkpoint schema's families (R4.B2, RULING-60 Q1).
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
    pub const fn as_str(self) -> &'static str {
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

    pub fn from_str_name(value: &str) -> Option<Self> {
        match value {
            "transcript" => Some(Self::Transcript),
            "run" => Some(Self::Run),
            "lifecycle" => Some(Self::Lifecycle),
            "dialogs" => Some(Self::Dialogs),
            "opaque" => Some(Self::Opaque),
            "active" => Some(Self::Active),
            "calls" => Some(Self::Calls),
            "settled" => Some(Self::Settled),
            "identity" => Some(Self::Identity),
            "source" => Some(Self::Source),
            _ => None,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FamilyRow {
    pub family: CatalogFamily,
    pub producer: FamilyProducer,
}

pub type FamilyProducers = BTreeMap<CatalogFamily, FamilyProducer>;

pub fn default_family_producers() -> FamilyProducers {
    CATALOG_FAMILIES
        .into_iter()
        .map(|f| (f, FamilyProducer::Complete))
        .collect()
}

pub fn entry_family(kind: &str) -> CatalogFamily {
    match kind {
        "session" | "message" => CatalogFamily::Transcript,
        "thinking_level_change"
        | "configuration_update"
        | "model_change"
        | "model_change_rejected"
        | "usage"
        | "compaction"
        | "branch_summary"
        | "custom"
        | "custom_message"
        | "context_edit"
        | "label"
        | "session_info" => CatalogFamily::Source,
        _ => CatalogFamily::Opaque,
    }
}

pub fn family_id(family: CatalogFamily) -> BoundedId {
    protocol_id(family.as_str())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssemblyState {
    Complete,
    Incomplete,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FamilyRegistry {
    pub format: String,
    pub version: u32,
    pub families: Vec<FamilyRow>,
    pub required_kinds: Vec<CatalogFamily>,
    pub assembly: AssemblyState,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub incomplete: Vec<CatalogFamily>,
}

pub fn family_registry(producers: &FamilyProducers) -> FamilyRegistry {
    let families: Vec<FamilyRow> = CATALOG_FAMILIES
        .iter()
        .map(|&family| FamilyRow {
            family,
            producer: producers
                .get(&family)
                .copied()
                .unwrap_or(FamilyProducer::Complete),
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
    let assembly = if incomplete.is_empty() {
        AssemblyState::Complete
    } else {
        AssemblyState::Incomplete
    };
    FamilyRegistry {
        format: "senpi-recovery-families".into(),
        version: 1,
        families,
        required_kinds,
        assembly,
        incomplete,
    }
}

/// A manifest string: inline when it is a bounded protocol id, otherwise a ref to its exact stored value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ManifestText {
    Id {
        id: BoundedId,
    },
    Ref {
        #[serde(rename = "ref")]
        ref_: ContentRef,
        length: usize,
        digest: ContentDigest,
    },
}

/// The compact-v1 manifest (R4.B1): bounded scalar identity of one frozen checkpoint and the root of its catalog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryManifest {
    pub format: String,
    pub version: u32,
    pub socket_epoch: ManifestText,
    pub admission_id: ManifestText,
    pub route_kind: RecoveryRouteKind,
    pub session: ManifestText,
    pub session_epoch: ManifestText,
    pub generation: u64,
    pub sequence: u64,
    pub history_version: u64,
    pub token: ManifestText,
    pub leaf: Option<ManifestText>,
    pub active_message: Option<ManifestText>,
    pub turn: Option<ManifestText>,
    pub checkpoint_digest: ContentDigest,
    pub catalog_root: ContentRef,
    pub catalog_root_length: usize,
    pub required_kinds: Vec<CatalogFamily>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TextField {
    SocketEpoch,
    AdmissionId,
    Session,
    SessionEpoch,
    Token,
    Leaf,
    ActiveMessage,
    Turn,
}

impl TextField {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SocketEpoch => "socketEpoch",
            Self::AdmissionId => "admissionId",
            Self::Session => "session",
            Self::SessionEpoch => "sessionEpoch",
            Self::Token => "token",
            Self::Leaf => "leaf",
            Self::ActiveMessage => "activeMessage",
            Self::Turn => "turn",
        }
    }
}

pub const TEXT_FIELDS: [TextField; 8] = [
    TextField::SocketEpoch,
    TextField::AdmissionId,
    TextField::Session,
    TextField::SessionEpoch,
    TextField::Token,
    TextField::Leaf,
    TextField::ActiveMessage,
    TextField::Turn,
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compact_recovery_ids::{catalog_root_ref, parse_content_digest};

    #[test]
    fn builds_family_registry_with_sorted_required_kinds() {
        let mut producers = default_family_producers();
        let full = family_registry(&producers);
        assert_eq!(full.assembly, AssemblyState::Complete);
        assert_eq!(full.required_kinds.len(), 10);
        // Sorted lexicographically: active, calls, dialogs, identity, lifecycle, opaque, run, settled, source, transcript
        assert_eq!(full.required_kinds[0], CatalogFamily::Active);
        assert_eq!(full.required_kinds[9], CatalogFamily::Transcript);

        producers.insert(CatalogFamily::Transcript, FamilyProducer::Partial);
        let partial = family_registry(&producers);
        assert_eq!(partial.assembly, AssemblyState::Incomplete);
        assert_eq!(partial.incomplete, vec![CatalogFamily::Transcript]);
        assert_eq!(partial.required_kinds.len(), 9);
    }

    #[test]
    fn manifest_serializes_with_expected_fields() {
        let manifest = RecoveryManifest {
            format: "senpi-recovery-manifest".into(),
            version: 1,
            socket_epoch: ManifestText::Id {
                id: protocol_id("se-1"),
            },
            admission_id: ManifestText::Id {
                id: protocol_id("adm-1"),
            },
            route_kind: RecoveryRouteKind::Ordinary,
            session: ManifestText::Id {
                id: protocol_id("sess-1"),
            },
            session_epoch: ManifestText::Id {
                id: protocol_id("epoch-1"),
            },
            generation: 1,
            sequence: 3,
            history_version: 2,
            token: ManifestText::Id {
                id: protocol_id("tok-1"),
            },
            leaf: None,
            active_message: None,
            turn: None,
            checkpoint_digest: parse_content_digest(&"a".repeat(64)).expect("digest"),
            catalog_root: catalog_root_ref(1),
            catalog_root_length: 128,
            required_kinds: vec![CatalogFamily::Active, CatalogFamily::Calls],
        };
        let v = serde_json::to_value(&manifest).expect("json");
        assert_eq!(v["format"], "senpi-recovery-manifest");
        assert_eq!(v["version"], 1);
        assert_eq!(v["socketEpoch"]["id"], "se-1");
        assert!(v["leaf"].is_null());
        assert_eq!(v["requiredKinds"], serde_json::json!(["active", "calls"]));
    }
}
