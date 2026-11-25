//! senpi `modes/rpc/compact-recovery-manifest.ts`: one manifest and catalog per token, built
//! once from the pinned root and released with the token (S09, R4.B1, RULING-69 (2)).

use crate::accepted_transcript::TranscriptRoot;
use crate::compact_content_store::{
    COMPACT_LEAF_CODE_UNITS, CompactContentStore, LeafId, utf16_len, utf16_slice,
};
use crate::compact_manifest_page::manifest_page;
use crate::compact_recovery_base::{base_stream, open_base_bytes};
use crate::compact_recovery_bytes::{
    CancelSignal, FrozenBytes, RecoveryBytesError, RecoveryPage, ValidCheck, produce_page,
};
use crate::compact_recovery_catalog::{
    CatalogComponent, CatalogRead, CatalogRoot, CompactRecoveryCatalog, component_bytes,
};
use crate::compact_recovery_ids::{
    ContentRef, PageRef, catalog_root_ref, child_ref, parse_bounded_id, parse_content_ref,
    parse_page_ref, protocol_id,
};
use crate::compact_recovery_manifest_shape::{
    CatalogFamily, FamilyRegistry, ManifestText, RecoveryManifest, TEXT_FIELDS, TextField,
    default_family_producers, family_registry,
};
use crate::compact_recovery_types::CheckpointToken;
use crate::compact_verified_components::{FrozenRootPinnedError, VerifiedComponents};
use futures::future::BoxFuture;
use futures::stream::{self, BoxStream, StreamExt};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

/// Callback producing family components and the verified `FamilyRegistry` for one frozen cut.
pub type FamilyComponentsFn = Arc<
    dyn Fn(
            CompactContentStore,
        ) -> BoxFuture<
            'static,
            Result<(Vec<CatalogComponent>, FamilyRegistry), RecoveryBytesError>,
        > + Send
        + Sync,
>;

#[derive(Clone)]
struct Built {
    manifest: RecoveryManifest,
    root: CatalogRoot,
    store: CompactContentStore,
}

fn utf16_chunks(value: &str) -> Vec<String> {
    let total = usize::try_from(utf16_len(value)).unwrap_or(0);
    let mut parts = Vec::new();
    let mut start = 0usize;
    while start < total {
        let slice = utf16_slice(value, start, start + COMPACT_LEAF_CODE_UNITS);
        let step = usize::try_from(utf16_len(slice)).unwrap_or(0).max(1);
        parts.push(slice.to_owned());
        start += step;
    }
    parts
}

/// One manifest and catalog per token, built once from the pinned root and released with the token.
pub struct RecoveryManifests {
    catalogs: CompactRecoveryCatalog,
    ready: Mutex<HashMap<String, Built>>,
    verified: Mutex<VerifiedComponents>,
    counter: Mutex<u64>,
    family_builder: Option<FamilyComponentsFn>,
}

impl Default for RecoveryManifests {
    fn default() -> Self {
        Self::new(None)
    }
}

impl RecoveryManifests {
    pub fn new(family_builder: Option<FamilyComponentsFn>) -> Self {
        Self {
            catalogs: CompactRecoveryCatalog::new(),
            ready: Mutex::new(HashMap::new()),
            verified: Mutex::new(VerifiedComponents::new()),
            counter: Mutex::new(0),
            family_builder,
        }
    }

    pub fn verified_components(&self) -> usize {
        self.verified
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .size()
    }

    /// `token_id` pins `root_key` for the cache; another live token pinning it is a `FrozenRootPinnedError`.
    pub fn pin(&self, token_id: &str, root_key: usize) -> Result<(), FrozenRootPinnedError> {
        self.verified
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pin(token_id, root_key)
    }

    pub async fn manifest(
        &self,
        token: &CheckpointToken,
        store: &CompactContentStore,
        valid: ValidCheck<'_>,
    ) -> Result<RecoveryManifest, RecoveryBytesError> {
        if let Some(built) = self
            .ready
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&token.id)
            .cloned()
        {
            return Ok(built.manifest);
        }
        let built = self.build(token, store, valid).await?;
        let manifest = built.manifest.clone();
        self.ready
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(token.id.clone(), built);
        Ok(manifest)
    }

    /// A raw ref is validated before any use; without a built catalog no ref can name anything.
    pub async fn read(
        &self,
        token_id: &str,
        ref_str: &str,
        valid: ValidCheck<'_>,
    ) -> Result<CatalogRead, RecoveryBytesError> {
        let parsed = parse_content_ref(ref_str).map_err(|_| RecoveryBytesError::Range)?;
        let built = self
            .ready
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(token_id)
            .cloned()
            .ok_or(RecoveryBytesError::Range)?;
        self.catalogs
            .read(&built.store, &built.root, &parsed, valid, None)
            .await
    }

    /// One page of a component's bytes at a raw byte offset, or of the cut's loaded base under its reserved ref.
    #[allow(clippy::too_many_arguments)]
    pub async fn page(
        &self,
        token: &CheckpointToken,
        store: &CompactContentStore,
        root_key: usize,
        transcript: &TranscriptRoot,
        ref_str: &str,
        offset: usize,
        valid: ValidCheck<'_>,
        signal: Option<&CancelSignal>,
    ) -> Result<RecoveryPage, RecoveryBytesError> {
        if ref_str == "manifest" {
            let manifest = self.manifest(token, store, valid).await?;
            return manifest_page(&manifest, offset);
        }
        let target = parse_page_ref(ref_str).map_err(|_| RecoveryBytesError::Range)?;
        let built = self
            .ready
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&token.id)
            .cloned()
            .ok_or(RecoveryBytesError::Range)?;

        let cached = self
            .verified
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(root_key, target.as_str());

        let bytes: Arc<dyn FrozenBytes> = match cached {
            Some(bytes) => bytes,
            None => {
                let opened: Arc<dyn FrozenBytes> = match &target {
                    PageRef::Base(bref) => {
                        let base = base_stream(transcript, &built.manifest.catalog_root, bref)?;
                        open_base_bytes(base, valid, signal, None, None).await?
                    }
                    PageRef::Catalog(cref) => {
                        let read = self
                            .catalogs
                            .read(&built.store, &built.root, cref, valid, signal)
                            .await?;
                        component_bytes(read)?
                    }
                };
                let still_current = self
                    .ready
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .get(&token.id)
                    .is_some_and(|b| b.root.ref_ == built.root.ref_);
                if still_current {
                    let _ = self
                        .verified
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .set(&token.id, root_key, target.as_str(), opened.clone());
                }
                opened
            }
        };
        produce_page(bytes.as_ref(), offset, Some(valid)).await
    }

    pub fn release(&self, token_id: &str) {
        let built = self
            .ready
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(token_id);
        self.verified
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .drop_token(token_id);
        if let Some(built) = built {
            self.catalogs.release(&built.store, &built.root);
        }
    }

    async fn build(
        &self,
        token: &CheckpointToken,
        store: &CompactContentStore,
        valid: ValidCheck<'_>,
    ) -> Result<Built, RecoveryBytesError> {
        let next_counter = {
            let mut guard = self.counter.lock().unwrap_or_else(PoisonError::into_inner);
            *guard += 1;
            *guard
        };
        let root_ref = catalog_root_ref(next_counter);
        let manifest_kind = protocol_id("manifest");

        let value_of = |field: TextField| -> Option<&str> {
            match field {
                TextField::SocketEpoch => Some(&token.admission.socket_epoch),
                TextField::AdmissionId => Some(&token.admission.admission_id),
                TextField::Session => Some(&token.admission.session_id),
                TextField::SessionEpoch => Some(&token.admission.session_epoch),
                TextField::Token => Some(&token.id),
                TextField::Leaf => token.cut.leaf_id.as_deref(),
                TextField::ActiveMessage => token.cut.active_message_id.as_deref(),
                TextField::Turn => token.cut.turn_id.as_deref(),
            }
        };

        let mut texts: HashMap<TextField, Option<ManifestText>> = HashMap::new();
        let mut oversized: Vec<(TextField, CatalogComponent)> = Vec::new();
        let mut owned: Vec<LeafId> = Vec::new();

        let build_result: Result<Built, RecoveryBytesError> = async {
            for field in TEXT_FIELDS {
                let Some(value) = value_of(field) else {
                    texts.insert(field, None);
                    continue;
                };
                if let Ok(bounded) = parse_bounded_id(value) {
                    texts.insert(field, Some(ManifestText::Id { id: bounded }));
                    continue;
                }
                let mut leaves = Vec::new();
                for chunk in utf16_chunks(value) {
                    let leaf = store.create_leaf(&chunk, false)?;
                    owned.push(leaf);
                    leaves.push(leaf);
                }
                let owner = store.text().own(&leaves)?;
                let component = CatalogComponent {
                    kind: manifest_kind.clone(),
                    id: protocol_id(field.as_str()),
                    leaves,
                    owner,
                };
                oversized.push((field, component));
            }

            let (family_components, registry) = match &self.family_builder {
                Some(builder) => builder(store.clone()).await?,
                None => (Vec::new(), family_registry(&default_family_producers())),
            };

            let mut all_components: Vec<CatalogComponent> =
                oversized.iter().map(|(_, c)| c.clone()).collect();
            all_components.extend(family_components);

            let mut component_stream: BoxStream<'_, CatalogComponent> =
                stream::iter(all_components).boxed();
            let built_root = self
                .catalogs
                .build(store, root_ref.clone(), &mut component_stream, valid)
                .await?;

            self.assemble(
                store,
                token,
                root_ref,
                built_root,
                &oversized,
                &mut texts,
                registry.required_kinds,
            )
            .await
        }
        .await;

        for (_, comp) in &oversized {
            let _ = store.adjust(comp.owner, -1, 0);
        }
        for leaf in owned {
            let _ = store.adjust(leaf, -1, 0);
        }
        build_result
    }

    #[allow(clippy::too_many_arguments)]
    async fn assemble(
        &self,
        store: &CompactContentStore,
        token: &CheckpointToken,
        ref_: ContentRef,
        built: CatalogRoot,
        oversized: &[(TextField, CatalogComponent)],
        texts: &mut HashMap<TextField, Option<ManifestText>>,
        required_kinds: Vec<CatalogFamily>,
    ) -> Result<Built, RecoveryBytesError> {
        let ok_valid = || Ok(());
        let res: Result<Built, RecoveryBytesError> = async {
            let mut leftmost = ref_.clone();
            for _ in 1..built.depth {
                leftmost = child_ref(&leftmost, 0);
            }
            if !oversized.is_empty() {
                let node = self
                    .catalogs
                    .read(store, &built, &leftmost, &ok_valid, None)
                    .await?;
                let CatalogRead::Node { entries, .. } = node else {
                    return Err(RecoveryBytesError::Ownership);
                };
                for (index, (field, component)) in oversized.iter().enumerate() {
                    let Some(entry) = entries.get(index) else {
                        return Err(RecoveryBytesError::Ownership);
                    };
                    if entry.kind != component.kind || entry.id != component.id {
                        return Err(RecoveryBytesError::Ownership);
                    }
                    texts.insert(
                        *field,
                        Some(ManifestText::Ref {
                            ref_: child_ref(&leftmost, index),
                            length: entry.length,
                            digest: entry.digest.clone(),
                        }),
                    );
                }
            }

            let take_opt = |texts: &mut HashMap<TextField, Option<ManifestText>>,
                            f: TextField|
             -> Result<Option<ManifestText>, RecoveryBytesError> {
                texts.remove(&f).ok_or(RecoveryBytesError::Ownership)
            };
            let take_req = |texts: &mut HashMap<TextField, Option<ManifestText>>,
                            f: TextField|
             -> Result<ManifestText, RecoveryBytesError> {
                take_opt(texts, f)?.ok_or(RecoveryBytesError::Ownership)
            };

            let manifest = RecoveryManifest {
                format: "senpi-recovery-manifest".into(),
                version: 1,
                socket_epoch: take_req(texts, TextField::SocketEpoch)?,
                admission_id: take_req(texts, TextField::AdmissionId)?,
                route_kind: token.admission.route_kind,
                session: take_req(texts, TextField::Session)?,
                session_epoch: take_req(texts, TextField::SessionEpoch)?,
                generation: token.admission.generation,
                sequence: token.cut.sequence,
                history_version: token.cut.history_version,
                token: take_req(texts, TextField::Token)?,
                leaf: take_opt(texts, TextField::Leaf)?,
                active_message: take_opt(texts, TextField::ActiveMessage)?,
                turn: take_opt(texts, TextField::Turn)?,
                checkpoint_digest: built.digest.clone(),
                catalog_root: ref_,
                catalog_root_length: built.length,
                required_kinds,
            };
            Ok(Built {
                manifest,
                root: built.clone(),
                store: store.clone(),
            })
        }
        .await;
        if res.is_err() {
            self.catalogs.release(store, &built);
        }
        res
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accepted_transcript::AcceptedTranscript;
    use crate::compact_content_store::CompactContentStoreOptions;
    use crate::compact_recovery_bytes::stored_bytes;
    use crate::compact_recovery_types::{
        RecoveryAdmission, RecoveryCutIdentity, RecoveryRouteKind,
    };

    fn ok_valid() -> Result<(), RecoveryBytesError> {
        Ok(())
    }

    fn token(session_id: &str) -> CheckpointToken {
        CheckpointToken::new(
            "tok-1".into(),
            RecoveryAdmission {
                socket_epoch: "se-1".into(),
                admission_id: "adm-1".into(),
                route_kind: RecoveryRouteKind::Ordinary,
                peer: "peer-1".into(),
                session_id: session_id.into(),
                session_epoch: "ep-1".into(),
                generation: 2,
            },
            RecoveryCutIdentity {
                session_epoch: "ep-1".into(),
                sequence: 5,
                history_version: 3,
                leaf_id: Some("leaf-1".into()),
                active_message_id: None,
                turn_id: None,
            },
        )
    }

    #[tokio::test]
    async fn builds_manifest_with_oversized_session_id_as_catalog_component_and_pages_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = CompactContentStore::new(CompactContentStoreOptions {
            dir: dir.path().join("epoch").display().to_string(),
            ..Default::default()
        });
        let manifests = RecoveryManifests::default();
        let long_session = format!("session/{}", "x".repeat(150));
        let tok = token(&long_session);
        manifests.pin(&tok.id, 1).expect("pin");
        let manifest = manifests
            .manifest(&tok, &store, &ok_valid)
            .await
            .expect("manifest");
        assert!(matches!(manifest.socket_epoch, ManifestText::Id { .. }));
        let ref_str = match &manifest.session {
            ManifestText::Ref { ref_, length, .. } => {
                assert_eq!(*length, long_session.len() * 2);
                ref_.as_str().to_owned()
            }
            ManifestText::Id { .. } => panic!("expected oversized session to become ref"),
        };

        let transcript = AcceptedTranscript::default().freeze();
        let m_page = manifests
            .page(&tok, &store, 1, &transcript, "manifest", 0, &ok_valid, None)
            .await
            .expect("manifest page");
        assert!(m_page.complete);

        let s_page = manifests
            .page(&tok, &store, 1, &transcript, &ref_str, 0, &ok_valid, None)
            .await
            .expect("session page");
        assert_eq!(s_page.bytes, stored_bytes(&long_session));
        assert_eq!(manifests.verified_components(), 1);

        manifests.release(&tok.id);
        assert_eq!(manifests.verified_components(), 0);
        store.idle().await;
        store.retire().await.expect("retire");
    }
}
