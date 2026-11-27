//! senpi `modes/rpc/compact-transcript-flows.ts`: maps a committed transcript flow into
//! compact message IDs (S03b-3b) using the epoch's settled entry-to-message joins.

use crate::compact_cold_flows::{BranchInstalledEntry, CompactColdFlow, SeededMessage};
use crate::compact_transition::CompactPersistence;
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstalledTranscriptEntry {
    pub id: String,
    pub message: SeededMessage,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum TranscriptFlow {
    Edit {
        target_id: String,
        message: SeededMessage,
    },
    ContextEditRemoval {
        target_id: String,
    },
    Compaction {
        removed_ids: Vec<String>,
    },
    Branch {
        removed_ids: Vec<String>,
        installed: Vec<InstalledTranscriptEntry>,
    },
}

pub type MessagesOfFn<'a> =
    &'a (dyn for<'r> Fn(&'r [String]) -> BoxFuture<'r, Vec<Option<String>>> + Send + Sync);

async fn known_ids(ids: &[String], messages_of: MessagesOfFn<'_>) -> Vec<String> {
    messages_of(ids)
        .await
        .into_iter()
        .flatten()
        .collect::<Vec<String>>()
}

/// A committed transcript flow in compact message ids (S03b-3b). An entry no message of this
/// epoch joined (the loaded base, an unowned append) was never a compact message.
pub async fn compact_cold_flow(
    flow: &TranscriptFlow,
    messages_of: MessagesOfFn<'_>,
) -> Option<CompactColdFlow> {
    match flow {
        TranscriptFlow::Edit { target_id, message } => {
            let mapped = messages_of(std::slice::from_ref(target_id)).await;
            let message_id = mapped.into_iter().next().flatten()?;
            Some(CompactColdFlow::Edit {
                message_id,
                message: message.clone(),
            })
        }
        TranscriptFlow::ContextEditRemoval { target_id } => {
            let mapped = messages_of(std::slice::from_ref(target_id)).await;
            let message_id = mapped.into_iter().next().flatten()?;
            Some(CompactColdFlow::ContextEditRemoval { message_id })
        }
        TranscriptFlow::Compaction { removed_ids } => Some(CompactColdFlow::Compaction {
            removed: known_ids(removed_ids, messages_of).await,
        }),
        TranscriptFlow::Branch {
            removed_ids,
            installed,
        } => {
            let entry_ids: Vec<String> = installed.iter().map(|e| e.id.clone()).collect();
            let installed_ids = messages_of(&entry_ids).await;
            let mut mapped_installed = Vec::new();
            for (entry, msg_id_opt) in installed.iter().zip(installed_ids) {
                if let Some(message_id) = msg_id_opt {
                    mapped_installed.push(BranchInstalledEntry {
                        message_id,
                        message: entry.message.clone(),
                        persistence: CompactPersistence::Appended,
                    });
                }
            }
            Some(CompactColdFlow::Branch {
                removed: known_ids(removed_ids, messages_of).await,
                installed: mapped_installed,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn maps_known_entries_and_drops_unjoined_ones() {
        fn lookup(ids: &[String]) -> BoxFuture<'_, Vec<Option<String>>> {
            let ids = ids.to_vec();
            Box::pin(async move {
                ids.into_iter()
                    .map(|id| match id.as_str() {
                        "e1" => Some("m1".into()),
                        "e2" => Some("m2".into()),
                        _ => None,
                    })
                    .collect()
            })
        }
        let lookup_fn: MessagesOfFn<'_> = &lookup;

        assert_eq!(
            compact_cold_flow(
                &TranscriptFlow::ContextEditRemoval {
                    target_id: "base-0".into()
                },
                lookup_fn
            )
            .await,
            None
        );

        let comp = compact_cold_flow(
            &TranscriptFlow::Compaction {
                removed_ids: vec!["base-0".into(), "e1".into()],
            },
            lookup_fn,
        )
        .await
        .expect("compaction");
        assert_eq!(
            comp,
            CompactColdFlow::Compaction {
                removed: vec!["m1".into()]
            }
        );

        let branch = compact_cold_flow(
            &TranscriptFlow::Branch {
                removed_ids: vec!["e1".into()],
                installed: vec![
                    InstalledTranscriptEntry {
                        id: "e2".into(),
                        message: json!({"role":"assistant"}),
                    },
                    InstalledTranscriptEntry {
                        id: "unowned".into(),
                        message: json!({"role":"user"}),
                    },
                ],
            },
            lookup_fn,
        )
        .await
        .expect("branch");
        match branch {
            CompactColdFlow::Branch { removed, installed } => {
                assert_eq!(removed, vec!["m1"]);
                assert_eq!(installed.len(), 1);
                assert_eq!(installed[0].message_id, "m2");
                assert_eq!(installed[0].persistence, CompactPersistence::Appended);
            }
            _ => panic!("expected branch"),
        }
    }
}
