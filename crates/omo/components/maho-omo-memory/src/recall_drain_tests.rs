use super::*;
use memory_core::identity::layout::build_identity_paths;
use memory_core::recall::RecallNudge;

struct FixedPending(Vec<RecallNudge>);

impl PendingNudgesPort for FixedPending {
    fn take(&self, _session_id: &str) -> Vec<RecallNudge> {
        self.0.clone()
    }
}

fn context(root: &std::path::Path) -> MemoryIdentityContext {
    MemoryIdentityContext::new(
        "agent".into(),
        build_identity_paths(root, "agent"),
        crate::binding::MemorySessionBinding { identity: "agent".into(), repo_path_hash: "hash".into(), bound_at: 0.0 },
    )
}

fn view(
    context: MemoryIdentityContext,
    ledger_dir: std::path::PathBuf,
    pending: Vec<RecallNudge>,
    queued: Vec<RecallNudge>,
    env: EnvLookup,
) -> RecallDrainOptionsView {
    RecallDrainOptionsView {
        resolve_context: Arc::new(move |_| Some(context.clone())),
        resolve_settings: Arc::new(|| serde_json::json!({ "recall": { "enabled": true } })),
        env,
        ledger_for: Arc::new(move |_| RecallLedger::new(ledger_dir.clone())),
        pending_for: Arc::new(move |_| Arc::new(FixedPending(pending.clone())) as Arc<dyn PendingNudgesPort>),
        drain_queued: if queued.is_empty() { None } else { let queued = queued.clone(); Some(Arc::new(move |_, _| queued.clone())) },
        warn: Arc::new(|_| {}),
    }
}

fn nudge(path: &str, hint: &str) -> RecallNudge {
    RecallNudge { path: path.into(), hint: hint.into() }
}

#[test]
fn given_pending_nudges_when_delivered_then_one_message_is_built_and_the_ledger_is_marked() {
    let root = tempfile::tempdir().unwrap();
    let ledger_dir = root.path().join("ledger");
    let options = view(context(root.path()), ledger_dir.clone(), vec![nudge("a.md", "The note records the deploy gate.")], vec![], Arc::new(|_| None));
    let delivery = deliver(&options, "s1").unwrap();
    assert_eq!(delivery.record.via.as_deref(), Some("prompt"));
    assert_eq!(delivery.record.nudges.len(), 1);
    assert!(delivery.message_content.contains("<recalled-memory"));
    let ledger = RecallLedger::new(ledger_dir);
    assert_eq!(ledger.surfaced_paths("s1"), std::collections::BTreeSet::from(["a.md".to_string()]));
}

#[test]
fn given_queued_and_pending_nudges_when_delivered_then_the_queued_copy_wins() {
    let root = tempfile::tempdir().unwrap();
    let options = view(
        context(root.path()),
        root.path().join("ledger"),
        vec![nudge("a.md", "from file")],
        vec![nudge("a.md", "from queue")],
        Arc::new(|_| None),
    );
    let delivery = deliver(&options, "s1").unwrap();
    assert_eq!(delivery.record.nudges.len(), 1);
    assert_eq!(delivery.record.nudges[0].hint, "from queue");
}

#[test]
fn given_no_nudges_when_delivered_then_nothing_is_built() {
    let root = tempfile::tempdir().unwrap();
    let options = view(context(root.path()), root.path().join("ledger"), vec![], vec![], Arc::new(|_| None));
    assert!(deliver(&options, "s1").is_none());
}

#[test]
fn given_a_child_sentinel_when_delivered_then_nothing_is_built() {
    let root = tempfile::tempdir().unwrap();
    let options = view(
        context(root.path()),
        root.path().join("ledger"),
        vec![nudge("a.md", "The note records the deploy gate.")],
        vec![],
        Arc::new(|name| (name == "SENPI_MEMORY_REFLECTION").then(|| "1".to_string())),
    );
    assert!(deliver(&options, "s1").is_none());
}

#[test]
fn given_recall_disabled_when_delivered_then_nothing_is_built() {
    let root = tempfile::tempdir().unwrap();
    let mut options = view(context(root.path()), root.path().join("ledger"), vec![nudge("a.md", "The note records the deploy gate.")], vec![], Arc::new(|_| None));
    options.resolve_settings = Arc::new(|| serde_json::json!({ "recall": { "enabled": false } }));
    assert!(deliver(&options, "s1").is_none());
}
