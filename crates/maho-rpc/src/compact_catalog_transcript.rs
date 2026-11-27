//! senpi `modes/rpc/compact-catalog-transcript.ts`: the `transcript` and `opaque` families
//! (R4.B2, RULING-60 Q1, RULING-61).

use crate::accepted_transcript::TranscriptRoot;
use crate::compact_catalog_codec::{BoundedId, protocol_id};
use crate::compact_catalog_registry::{
    CatalogFamily, FamilyProducer, FamilyProducers, MemberOrdinal, MemberPrefix, entry_family,
    family_id, member_id,
};
use crate::compact_catalog_writer::{CatalogComponent, CatalogComponentSink, written_component};
use crate::compact_content_store::CompactContentStore;
use crate::compact_read_lease::CompactError;
use async_trait::async_trait;
use maho_core::session_transcript_journal::TranscriptPersistence;
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptedEntryMeta {
    pub index: usize,
    pub id: String,
    pub kind: String,
    pub parent_id: Option<String>,
}

/// A reader's consistent cut of one transcript tail (`accepted-transcript-tail.ts`).
#[async_trait]
pub trait TailHold: Send + Sync {
    fn persistence(&self) -> Option<&TranscriptPersistence>;
    fn has(&self, index: usize) -> bool;
    async fn chunks(
        &self,
        index: usize,
        valid: &(dyn Fn() -> Result<(), CompactError> + Sync),
    ) -> Result<Vec<String>, CompactError>;
    fn release(&mut self);
}

/// Default tail hold when no in-memory tail segments are held separately.
#[derive(Debug, Default)]
pub struct EmptyTailHold {
    pub persistence: Option<TranscriptPersistence>,
}

#[async_trait]
impl TailHold for EmptyTailHold {
    fn persistence(&self) -> Option<&TranscriptPersistence> {
        self.persistence.as_ref()
    }

    fn has(&self, _index: usize) -> bool {
        false
    }

    async fn chunks(
        &self,
        index: usize,
        _valid: &(dyn Fn() -> Result<(), CompactError> + Sync),
    ) -> Result<Vec<String>, CompactError> {
        Err(CompactError::Io(format!(
            "transcript tail entry {index} is not held"
        )))
    }

    fn release(&mut self) {}
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TranscriptBaseSummary {
    count: usize,
    id_fingerprint: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TranscriptDescriptor {
    history_version: u64,
    leaf_id: Option<String>,
    count: usize,
    base: Option<TranscriptBaseSummary>,
    persisted: Option<usize>,
    unavailable: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BaseRow {
    source: &'static str,
    first: usize,
    count: usize,
    id_fingerprint: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AncestryRow<'a> {
    index: usize,
    id: &'a str,
    source: &'static str,
    parent_id: Option<&'a str>,
    kind: &'a str,
    family: &'static str,
}

/// The base row's text when the root has a non-empty loaded base, else `None`.
pub fn base_row(transcript: &TranscriptRoot) -> Option<String> {
    if transcript.unavailable.is_some() {
        return None;
    }
    let base = transcript.base.as_ref()?;
    if base.count == 0 {
        return None;
    }
    serde_json::to_string(&BaseRow {
        source: "file",
        first: 0,
        count: base.count,
        id_fingerprint: base.id_fingerprint.clone(),
    })
    .ok()
}

pub async fn text_component(
    store: &CompactContentStore,
    kind: BoundedId,
    id: BoundedId,
    text: &str,
    sink: &mut dyn CatalogComponentSink,
) -> Result<(), CompactError> {
    written_component(store, kind, id, sink, |mut writer| async move {
        writer.push(text)?;
        Ok(writer)
    })
    .await
}

/// The transcript and opaque families (R4.B2, RULING-60 Q1, RULING-61).
pub async fn transcript_components(
    store: &CompactContentStore,
    transcript: &TranscriptRoot,
    accepted: &[AcceptedEntryMeta],
    hold: &mut dyn TailHold,
    producers: &FamilyProducers,
    valid: &(dyn Fn() -> Result<(), CompactError> + Sync),
    sink: &mut dyn CatalogComponentSink,
) -> Result<(), CompactError> {
    let result = held_components(store, transcript, accepted, hold, producers, valid, sink).await;
    hold.release();
    result
}

async fn held_components(
    store: &CompactContentStore,
    transcript: &TranscriptRoot,
    accepted: &[AcceptedEntryMeta],
    hold: &dyn TailHold,
    producers: &FamilyProducers,
    valid: &(dyn Fn() -> Result<(), CompactError> + Sync),
    sink: &mut dyn CatalogComponentSink,
) -> Result<(), CompactError> {
    let transcript_id = family_id(CatalogFamily::Transcript);
    let opaque_id = family_id(CatalogFamily::Opaque);
    let base_id = protocol_id("base");

    let unavailable = transcript.unavailable.map(|reason| {
        serde_json::to_value(reason)
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_else(|| format!("{reason:?}"))
    });
    let descriptor = TranscriptDescriptor {
        history_version: transcript.history_version,
        leaf_id: transcript.leaf_id.clone(),
        count: transcript.count,
        base: transcript.base.as_ref().map(|base| TranscriptBaseSummary {
            count: base.count,
            id_fingerprint: base.id_fingerprint.clone(),
        }),
        persisted: hold.persistence().map(|p| p.count),
        unavailable,
    };
    text_component(
        store,
        transcript_id.clone(),
        transcript_id.clone(),
        &serde_json::to_string(&descriptor)?,
        sink,
    )
    .await?;
    if transcript.unavailable.is_some() {
        return Ok(());
    }
    let base = base_row(transcript);
    if let Some(base_text) = &base {
        text_component(
            store,
            transcript_id.clone(),
            base_id.clone(),
            base_text,
            sink,
        )
        .await?;
    }
    let mut opaque_count = 0usize;
    for entry in accepted {
        valid()?;
        let tail = hold.has(entry.index);
        let family = entry_family(&entry.kind);
        let row = AncestryRow {
            index: entry.index,
            id: &entry.id,
            source: if tail { "tail" } else { "file" },
            parent_id: entry.parent_id.as_deref(),
            kind: &entry.kind,
            family: family.as_str(),
        };
        let line = serde_json::to_string(&row)?;
        let entry_id = member_id(
            MemberPrefix::Entry,
            MemberOrdinal::Number(u64::try_from(entry.index).unwrap_or(u64::MAX)),
        );
        text_component(store, transcript_id.clone(), entry_id.clone(), &line, sink).await?;
        if family != CatalogFamily::Opaque {
            continue;
        }
        opaque_count += 1;
        if tail {
            let chunks = hold.chunks(entry.index, valid).await?;
            written_component(
                store,
                opaque_id.clone(),
                entry_id,
                sink,
                |mut writer| async move {
                    for chunk in chunks {
                        writer.push(&chunk)?;
                    }
                    Ok(writer)
                },
            )
            .await?;
        } else {
            text_component(store, opaque_id.clone(), entry_id, &line, sink).await?;
        }
    }
    if let Some(base_text) = &base {
        text_component(store, opaque_id, base_id, base_text, sink).await?;
    } else if opaque_count == 0 && producers.opaque == FamilyProducer::Complete {
        sink.accept(CatalogComponent {
            kind: opaque_id.clone(),
            id: opaque_id,
            leaves: Vec::new(),
            owner: 0,
        })
        .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accepted_transcript::AcceptedTranscript;
    use crate::compact_catalog_registry::FAMILY_PRODUCERS;
    use crate::compact_content_store::CompactContentStoreOptions;
    use maho_core::session_transcript_journal::TranscriptTransition;

    #[tokio::test]
    async fn emits_transcript_descriptor_ancestry_and_explicit_empty_opaque() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = CompactContentStore::new(CompactContentStoreOptions {
            dir: dir.path().join("epoch").display().to_string(),
            ..Default::default()
        });
        let mut transcript = AcceptedTranscript::default();
        transcript.apply(&TranscriptTransition::Replace { base: None });
        let root = transcript.freeze();

        struct Collect(Vec<(String, String)>);
        #[async_trait]
        impl CatalogComponentSink for Collect {
            async fn accept(&mut self, component: CatalogComponent) -> Result<(), CompactError> {
                self.0.push((
                    component.kind.as_str().to_owned(),
                    component.id.as_str().to_owned(),
                ));
                Ok(())
            }
        }
        let mut sink = Collect(Vec::new());
        let mut hold = EmptyTailHold::default();
        let accepted = vec![AcceptedEntryMeta {
            index: 0,
            id: "e0".into(),
            kind: "message".into(),
            parent_id: None,
        }];
        let ok = || -> Result<(), CompactError> { Ok(()) };
        transcript_components(
            &store,
            &root,
            &accepted,
            &mut hold,
            &FAMILY_PRODUCERS,
            &ok,
            &mut sink,
        )
        .await
        .expect("transcript");
        assert_eq!(
            sink.0,
            vec![
                ("transcript".into(), "transcript".into()),
                ("transcript".into(), "entry-0000000000000000".into()),
                ("opaque".into(), "opaque".into()),
            ]
        );
        store.retire().await.expect("retire");
    }
}
