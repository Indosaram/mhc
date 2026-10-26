//! Project source mentions in recall transcript (#10044).
//!
//! The model reads a project note at its file path (the project nudge and the recall notice both
//! show that path), never at its `@project/<identity>/<path>` key, so a note it already read is
//! found in the transcript by the file path.

pub type SourceForCallback = Arc<dyn Fn(&str, &str) -> Option<String> + Send + Sync>;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use serde_json::Value;

use memory_core::recall::RecallDocument;
use crate::recall_transcript_mentions::{
    create_transcript_mention_index, BranchMentionIndex, MentionDocument, TranscriptMentionIndex,
    TranscriptMentionInput,
};

pub struct ProjectSourceMentions {
    source_for: SourceForCallback,
    index: BranchMentionIndex,
}

impl ProjectSourceMentions {
    pub fn new(source_for: SourceForCallback) -> Self {
        Self {
            source_for,
            index: create_transcript_mention_index(),
        }
    }

    pub fn excluded_paths(
        &mut self,
        session_id: &str,
        entries: &[Value],
        project_documents: &[RecallDocument],
    ) -> BTreeSet<String> {
        if project_documents.is_empty() {
            return BTreeSet::new();
        }
        let mut qualified: BTreeMap<String, String> = BTreeMap::new();
        let mut documents: Vec<MentionDocument> = Vec::new();
        for doc in project_documents {
            if let Some(source) = (self.source_for)(session_id, &doc.path) {
                qualified.insert(source.clone(), doc.path.clone());
                documents.push(MentionDocument { path: source });
            }
        }
        let input = TranscriptMentionInput {
            session_id,
            entries,
            documents: &documents,
        };
        let read = self.index.excluded_paths(input);
        let mut result = BTreeSet::new();
        for source in read {
            if let Some(qual) = qualified.get(&source) {
                result.insert(qual.clone());
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_documents_empty_exclusions() {
        let mut mentions = ProjectSourceMentions::new(Arc::new(|_, _| None));
        let result = mentions.excluded_paths("s1", &[], &[]);
        assert!(result.is_empty());
    }
}
