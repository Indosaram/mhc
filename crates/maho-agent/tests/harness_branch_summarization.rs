//! Port of senpi packages/agent/test/harness/branch-summarization.test.ts (the entry-collection cases).

mod support;

use std::collections::BTreeMap;
use std::sync::Arc;

use maho_agent::harness::compaction::branch_summarization::collect_entries_for_branch_summary;
use maho_agent::harness::context::{BACKGROUND_CONTEXT, Context};
use maho_agent::harness::session::session::SessionError;
use maho_agent::harness::session::types::{
    Branch, BranchScan, Entry, EntryQuery, IdGenerator, JsonValue, NewEntry, Session, SessionMetadata,
    SessionMutation, SessionMutationCallback, SessionReader, SessionStats, StorageBranchScan,
};
use maho_agent::harness::session::values::{ListElement, ListReadOptions, StoredValue, Value, ValueList};
use maho_agent::types::AgentMessage;
use maho_ai::types::{BoxFuture, ContentBlock, Message, UserContent, UserMessage};

fn message(text: &str) -> AgentMessage {
    AgentMessage::Llm(Message::User(UserMessage {
        content: UserContent::Blocks(vec![ContentBlock::text(text)]),
        timestamp: 1,
    }))
}

fn message_entry(id: &str, parent_id: Option<&str>, text: &str, seq: i64) -> Entry {
    let entry = NewEntry::message(id, parent_id.map(str::to_owned), message(text));
    Entry {
        id: entry.id,
        parent_id: entry.parent_id,
        seq,
        timestamp: seq,
        kind: entry.kind,
    }
}

/// In-memory entry reader shared by the fake Branch and Session.
struct EntryIndex {
    by_id: BTreeMap<String, Entry>,
}

impl EntryIndex {
    fn new(entries: Vec<Entry>) -> Arc<Self> {
        Arc::new(Self {
            by_id: entries.into_iter().map(|entry| (entry.id.clone(), entry)).collect(),
        })
    }

    fn path_from(&self, start: &str) -> Result<Vec<Entry>, SessionError> {
        let mut path = Vec::new();
        let mut current = Some(start.to_owned());
        while let Some(id) = current {
            let entry = self.by_id.get(&id).cloned().ok_or_else(|| {
                SessionError::new(
                    maho_agent::harness::session::session::SessionErrorKind::Invariant,
                    format!("Unknown entry {id}"),
                )
            })?;
            current = entry.parent_id.clone();
            path.push(entry);
        }
        Ok(path)
    }
}

struct FakeBranch {
    index: Arc<EntryIndex>,
}

impl Branch for FakeBranch {
    fn name(&self) -> &str {
        "main"
    }

    fn get_tip_id<'a>(&'a self, _context: &'a Context) -> BoxFuture<'a, Result<Option<String>, SessionError>> {
        Box::pin(async move { Ok(None) })
    }

    fn find_entries<'a>(
        &'a self,
        query: Option<BranchScan>,
        _context: &'a Context,
    ) -> BoxFuture<'a, Result<Vec<Entry>, SessionError>> {
        Box::pin(async move {
            let start = query.and_then(|query| query.start);
            match start {
                Some(start) => self.index.path_from(&start),
                None => Ok(Vec::new()),
            }
        })
    }

    fn find_entry<'a>(
        &'a self,
        _query: Option<BranchScan>,
        _context: &'a Context,
    ) -> BoxFuture<'a, Result<Option<Entry>, SessionError>> {
        Box::pin(async move { Ok(None) })
    }

    fn append_message<'a>(
        &'a self,
        _message: AgentMessage,
        _context: &'a Context,
    ) -> BoxFuture<'a, Result<String, SessionError>> {
        Box::pin(async move { Ok(String::new()) })
    }

    fn append_custom_entry<'a>(
        &'a self,
        _custom_type: String,
        _data: Option<JsonValue>,
        _context: &'a Context,
    ) -> BoxFuture<'a, Result<String, SessionError>> {
        Box::pin(async move { Ok(String::new()) })
    }
}

struct FakeSession {
    index: Arc<EntryIndex>,
    metadata: SessionMetadata,
    id_generator: IdGenerator,
}

impl SessionReader for FakeSession {
    fn get_entries<'a>(
        &'a self,
        ids: Vec<String>,
        _context: &'a Context,
    ) -> BoxFuture<'a, Result<BTreeMap<String, Entry>, SessionError>> {
        Box::pin(async move {
            Ok(ids
                .into_iter()
                .filter_map(|id| self.index.by_id.get(&id).map(|entry| (id, entry.clone())))
                .collect())
        })
    }

    fn get_stats<'a>(&'a self, _context: &'a Context) -> BoxFuture<'a, Result<SessionStats, SessionError>> {
        Box::pin(async move {
            Ok(SessionStats {
                message_count: 0,
                usage: maho_agent::harness::utils::usage::empty_usage(),
            })
        })
    }

    fn get_value<'a>(
        &'a self,
        _address: &'a Value,
        _context: &'a Context,
    ) -> BoxFuture<'a, Result<Option<StoredValue>, SessionError>> {
        Box::pin(async move { Ok(None) })
    }

    fn scan_values<'a>(
        &'a self,
        _prefix: &'a Value,
        _context: &'a Context,
    ) -> BoxFuture<'a, Result<Vec<StoredValue>, SessionError>> {
        Box::pin(async move { Ok(Vec::new()) })
    }

    fn read_list<'a>(
        &'a self,
        _address: &'a ValueList,
        _options: Option<ListReadOptions>,
        _context: &'a Context,
    ) -> BoxFuture<'a, Result<Vec<ListElement>, SessionError>> {
        Box::pin(async move { Ok(Vec::new()) })
    }

    fn scan_branch<'a>(
        &'a self,
        _query: StorageBranchScan,
        _context: &'a Context,
    ) -> BoxFuture<'a, Result<Vec<Entry>, SessionError>> {
        Box::pin(async move { Ok(Vec::new()) })
    }
}

impl Session for FakeSession {
    fn metadata(&self) -> &SessionMetadata {
        &self.metadata
    }

    fn id_generator(&self) -> &IdGenerator {
        &self.id_generator
    }

    fn get_entry<'a>(
        &'a self,
        id: &'a str,
        _context: &'a Context,
    ) -> BoxFuture<'a, Result<Option<Entry>, SessionError>> {
        Box::pin(async move { Ok(self.index.by_id.get(id).cloned()) })
    }

    fn get_name<'a>(&'a self, _context: &'a Context) -> BoxFuture<'a, Result<Option<String>, SessionError>> {
        Box::pin(async move { Ok(None) })
    }

    fn get_label<'a>(
        &'a self,
        _target_id: &'a str,
        _context: &'a Context,
    ) -> BoxFuture<'a, Result<Option<String>, SessionError>> {
        Box::pin(async move { Ok(None) })
    }

    fn find_entries<'a>(
        &'a self,
        _query: Option<EntryQuery>,
        _context: &'a Context,
    ) -> BoxFuture<'a, Result<Vec<Entry>, SessionError>> {
        Box::pin(async move { Ok(Vec::new()) })
    }

    fn find_entry<'a>(
        &'a self,
        _query: Option<EntryQuery>,
        _context: &'a Context,
    ) -> BoxFuture<'a, Result<Option<Entry>, SessionError>> {
        Box::pin(async move { Ok(None) })
    }

    fn branch<'a>(
        &'a self,
        _name: &'a str,
        _context: &'a Context,
    ) -> BoxFuture<'a, Result<Option<Box<dyn Branch>>, SessionError>> {
        Box::pin(async move { Ok(None) })
    }

    fn create_branch<'a>(
        &'a self,
        _name: &'a str,
        _at: Option<String>,
        _context: &'a Context,
    ) -> BoxFuture<'a, Result<Box<dyn Branch>, SessionError>> {
        Box::pin(async move {
            Err(SessionError::new(
                maho_agent::harness::session::session::SessionErrorKind::Invariant,
                "unsupported",
            ))
        })
    }

    fn begin_mutation<'a>(
        &'a self,
        _context: &'a Context,
    ) -> BoxFuture<'a, Result<Box<dyn SessionMutation>, SessionError>> {
        Box::pin(async move {
            Err(SessionError::new(
                maho_agent::harness::session::session::SessionErrorKind::Invariant,
                "unsupported",
            ))
        })
    }

    fn mutate<'a>(
        &'a self,
        _mutation: SessionMutationCallback,
        _context: &'a Context,
    ) -> BoxFuture<'a, Result<JsonValue, SessionError>> {
        Box::pin(async move { Ok(serde_json::Value::Null) })
    }

    fn set_value<'a>(
        &'a self,
        _address: &'a Value,
        _next: JsonValue,
        _context: &'a Context,
    ) -> BoxFuture<'a, Result<(), SessionError>> {
        Box::pin(async move { Ok(()) })
    }

    fn delete_value<'a>(&'a self, _address: &'a Value, _context: &'a Context) -> BoxFuture<'a, Result<(), SessionError>> {
        Box::pin(async move { Ok(()) })
    }

    fn append_list<'a>(
        &'a self,
        _address: &'a ValueList,
        _element: JsonValue,
        _context: &'a Context,
    ) -> BoxFuture<'a, Result<(), SessionError>> {
        Box::pin(async move { Ok(()) })
    }

    fn delete_list<'a>(&'a self, _address: &'a ValueList, _context: &'a Context) -> BoxFuture<'a, Result<(), SessionError>> {
        Box::pin(async move { Ok(()) })
    }

    fn set_name<'a>(&'a self, _name: Option<String>, _context: &'a Context) -> BoxFuture<'a, Result<(), SessionError>> {
        Box::pin(async move { Ok(()) })
    }

    fn set_label<'a>(
        &'a self,
        _target_id: &'a str,
        _label: Option<String>,
        _context: &'a Context,
    ) -> BoxFuture<'a, Result<(), SessionError>> {
        Box::pin(async move { Ok(()) })
    }

    fn close<'a>(&'a self, _context: &'a Context) -> BoxFuture<'a, ()> {
        Box::pin(async move {})
    }
}

fn reader(entries: Vec<Entry>) -> (FakeBranch, FakeSession) {
    let index = EntryIndex::new(entries);
    (
        FakeBranch {
            index: index.clone(),
        },
        FakeSession {
            index,
            metadata: SessionMetadata {
                id: "session".to_owned(),
                created_at: 1,
                storage_version: 1,
                cwd: None,
                parent_session_id: None,
                legacy_parent_session_path: None,
            },
            id_generator: Arc::new(|_| "generated".to_owned()),
        },
    )
}

#[tokio::test]
async fn collects_the_abandoned_side_of_a_branch_in_chronological_order() {
    let context = BACKGROUND_CONTEXT.clone();
    let root = message_entry("root", None, "root", 1);
    let common = message_entry("common", Some("root"), "common", 2);
    let abandoned1 = message_entry("abandoned-1", Some("common"), "abandoned 1", 3);
    let abandoned2 = message_entry("abandoned-2", Some("abandoned-1"), "abandoned 2", 4);
    let target = message_entry("target", Some("common"), "target", 5);
    let (branch, session) = reader(vec![root, common, abandoned1, abandoned2, target]);

    let result = collect_entries_for_branch_summary(&branch, &session, Some("abandoned-2"), "target", &context)
        .await
        .expect("collect");
    assert_eq!(result.common_ancestor_id.as_deref(), Some("common"));
    assert_eq!(
        result.entries.iter().map(|entry| entry.id.as_str()).collect::<Vec<&str>>(),
        vec!["abandoned-1", "abandoned-2"]
    );
    assert!(!result.entries.iter().any(|entry| entry.id == "root"));
}

#[tokio::test]
async fn returns_no_entries_when_there_was_no_previous_leaf() {
    let context = BACKGROUND_CONTEXT.clone();
    let target = message_entry("target", None, "target", 1);
    let (branch, session) = reader(vec![target]);

    let result = collect_entries_for_branch_summary(&branch, &session, None, "target", &context)
        .await
        .expect("collect");
    assert!(result.entries.is_empty());
    assert!(result.common_ancestor_id.is_none());
}

#[test]
fn prepare_branch_entries_accumulates_file_operations_from_prior_summaries() {
    use maho_agent::harness::compaction::branch_summarization::prepare_branch_entries;
    use maho_agent::harness::session::types::EntryKind;

    let mut summary = NewEntry::branch_summary("b1", None, None, "prior", false);
    summary = summary.with_details(serde_json::json!({
        "readFiles": ["/read.txt"],
        "modifiedFiles": ["/edited.txt"],
    }));
    let summary = Entry {
        id: summary.id,
        parent_id: summary.parent_id,
        seq: 1,
        timestamp: 1,
        kind: summary.kind,
    };
    let message = message_entry("m1", Some("b1"), "hello", 2);
    assert!(matches!(summary.kind, EntryKind::BranchSummary { .. }));

    let preparation = prepare_branch_entries(&[summary, message], 0);
    assert_eq!(preparation.messages.len(), 2);
    assert!(preparation.file_ops.read.contains("/read.txt"));
    assert!(preparation.file_ops.edited.contains("/edited.txt"));
    assert!(preparation.total_tokens > 0);
}
