//! senpi `modes/rpc/compact-manifest-page.ts`: serves the bounded manifest itself under its
//! token on the one S11 page route (`componentRef: "manifest"`).

use crate::compact_recovery_bytes::{
    CatalogDomain, RecoveryBytesError, RecoveryPage, catalog_digest, stored_bytes,
};
use crate::compact_recovery_manifest_shape::RecoveryManifest;

pub const MANIFEST_PAGE_MAX_BYTES: usize = 65_536;

/// The bounded manifest itself is discoverable under its token on the one S11 page route.
pub fn manifest_page(
    manifest: &RecoveryManifest,
    offset: usize,
) -> Result<RecoveryPage, RecoveryBytesError> {
    if offset != 0 {
        return Err(RecoveryBytesError::Range);
    }
    let json =
        serde_json::to_string(manifest).map_err(|e| RecoveryBytesError::Io(e.to_string()))?;
    let bytes = stored_bytes(&json);
    if bytes.len() > MANIFEST_PAGE_MAX_BYTES {
        return Err(RecoveryBytesError::SizeLimit);
    }
    let length = bytes.len();
    let digest = catalog_digest(CatalogDomain::Fragment, &bytes);
    Ok(RecoveryPage {
        offset: 0,
        length,
        bytes,
        digest,
        next: None,
        complete: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compact_recovery_ids::{catalog_root_ref, parse_content_digest, protocol_id};
    use crate::compact_recovery_manifest_shape::{CatalogFamily, ManifestText};
    use crate::compact_recovery_types::RecoveryRouteKind;

    fn sample_manifest() -> RecoveryManifest {
        RecoveryManifest {
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
                id: protocol_id("ep-1"),
            },
            generation: 1,
            sequence: 1,
            history_version: 1,
            token: ManifestText::Id {
                id: protocol_id("tok-1"),
            },
            leaf: None,
            active_message: None,
            turn: None,
            checkpoint_digest: parse_content_digest(&"b".repeat(64)).expect("digest"),
            catalog_root: catalog_root_ref(1),
            catalog_root_length: 100,
            required_kinds: vec![CatalogFamily::Active],
        }
    }

    #[test]
    fn produces_complete_manifest_page_at_offset_zero_and_refuses_nonzero_offset() {
        let manifest = sample_manifest();
        let page = manifest_page(&manifest, 0).expect("page");
        assert!(page.complete);
        assert_eq!(page.offset, 0);
        assert_eq!(page.next, None);
        assert_eq!(
            page.digest,
            catalog_digest(CatalogDomain::Fragment, &page.bytes)
        );
        assert_eq!(manifest_page(&manifest, 2), Err(RecoveryBytesError::Range));
    }
}
