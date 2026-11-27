//! senpi `modes/rpc/compact-recovery-manifest-kinds.ts`: validates the `requiredKinds` field of
//! a decoded manifest against the cut's verified `families/families` registry (RULING-128).

use crate::compact_recovery_manifest_shape::{CatalogFamily, FamilyRegistry};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequiredKindsRefusal {
    ManifestMissingRequiredKinds,
    ManifestUnknownRequiredKind,
    ManifestRequiredKindsMismatch,
}

impl RequiredKindsRefusal {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ManifestMissingRequiredKinds => "manifest_missing_required_kinds",
            Self::ManifestUnknownRequiredKind => "manifest_unknown_required_kind",
            Self::ManifestRequiredKindsMismatch => "manifest_required_kinds_mismatch",
        }
    }
}

pub type RequiredKindsRead = Result<Vec<CatalogFamily>, RequiredKindsRefusal>;

/// The families a client opens for one decoded manifest (RULING-128): its `requiredKinds`, read
/// verbatim and never replaced by a built-in family list.
pub fn read_required_kinds(manifest: &Value, registry: &FamilyRegistry) -> RequiredKindsRead {
    let Some(listed) = manifest.get("requiredKinds").and_then(Value::as_array) else {
        return Err(RequiredKindsRefusal::ManifestMissingRequiredKinds);
    };
    let mut parsed = Vec::with_capacity(listed.len());
    for item in listed {
        let Some(kind) = item.as_str().and_then(CatalogFamily::from_str_name) else {
            return Err(RequiredKindsRefusal::ManifestUnknownRequiredKind);
        };
        parsed.push(kind);
    }
    if parsed != registry.required_kinds {
        return Err(RequiredKindsRefusal::ManifestRequiredKindsMismatch);
    }
    Ok(registry.required_kinds.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compact_recovery_manifest_shape::{
        FamilyProducer, default_family_producers, family_registry,
    };
    use serde_json::json;

    #[test]
    fn validates_required_kinds_against_registry() {
        let registry = family_registry(&default_family_producers());
        assert_eq!(
            read_required_kinds(&json!({}), &registry),
            Err(RequiredKindsRefusal::ManifestMissingRequiredKinds)
        );
        assert_eq!(
            read_required_kinds(&json!({"requiredKinds": ["bogus"]}), &registry),
            Err(RequiredKindsRefusal::ManifestUnknownRequiredKind)
        );
        assert_eq!(
            read_required_kinds(&json!({"requiredKinds": ["active"]}), &registry),
            Err(RequiredKindsRefusal::ManifestRequiredKindsMismatch)
        );
        let ok = read_required_kinds(
            &json!({"requiredKinds": registry.required_kinds}),
            &registry,
        )
        .expect("ok");
        assert_eq!(ok.len(), 10);

        let mut partial_producers = default_family_producers();
        partial_producers.insert(CatalogFamily::Transcript, FamilyProducer::Partial);
        let partial = family_registry(&partial_producers);
        assert_eq!(
            read_required_kinds(&json!({"requiredKinds": registry.required_kinds}), &partial,),
            Err(RequiredKindsRefusal::ManifestRequiredKindsMismatch)
        );
    }
}
