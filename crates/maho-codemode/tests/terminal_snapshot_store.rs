use maho_codemode::tool::{
    detached_cell_contract::*, terminal_snapshot_store::*, types::EvalLanguage,
};
use maho_ext_api::AgentToolResult;

fn snapshot(id: &str) -> EvalDetachedCellSnapshot {
    EvalDetachedCellSnapshot {
        cell_id: id.into(),
        language: EvalLanguage::Js,
        started_at_ms: 0.0,
        state: EvalDetachedCellState::Completed,
        queued_behind: None,
        output_tail: String::new(),
        result: AgentToolResult::text(id),
        state_retained: None,
        interrupt_note: None,
        hard_limit_seconds: None,
        run_budget_seconds: None,
    }
}

#[test]
fn cap_evicts_oldest_and_get_does_not_refresh() {
    let mut store = TerminalSnapshotStore::new(2);
    store.remember(snapshot("a"));
    store.remember(snapshot("b"));
    assert!(store.get("a").is_some());
    store.remember(snapshot("c"));
    assert!(store.get("a").is_none());
    assert_eq!(
        store
            .list()
            .iter()
            .map(|snapshot| snapshot.cell_id.as_str())
            .collect::<Vec<_>>(),
        ["b", "c"]
    );
}

#[test]
fn replacement_refreshes_and_deletion_clear_release() {
    let mut store = TerminalSnapshotStore::new(2);
    store.remember(snapshot("a"));
    store.remember(snapshot("b"));
    store.remember(snapshot("a"));
    assert_eq!(
        store
            .list()
            .iter()
            .map(|snapshot| snapshot.cell_id.as_str())
            .collect::<Vec<_>>(),
        ["b", "a"]
    );
    store.delete("b");
    assert_eq!(store.list().len(), 1);
    store.clear();
    assert!(store.list().is_empty());
}

#[test]
fn newest_snapshot_is_always_kept_even_at_zero_capacity() {
    // senpi 903b8336 terminal-snapshot-store.ts: eviction stops at one snapshot.
    let mut store = TerminalSnapshotStore::new(0);
    store.remember(snapshot("a"));
    store.remember(snapshot("b"));
    assert_eq!(
        store
            .list()
            .iter()
            .map(|snapshot| snapshot.cell_id.as_str())
            .collect::<Vec<_>>(),
        ["b"]
    );
}

fn image_snapshot(id: &str, image: &str) -> EvalDetachedCellSnapshot {
    let mut snapshot = snapshot(id);
    snapshot.result.content = vec![
        maho_ai::types::ContentBlock::Text(maho_ai::types::TextContent {
            text: format!("output-{id}"),
            audience: None,
            text_signature: None,
        }),
        maho_ai::types::ContentBlock::Image(maho_ai::types::ImageContent {
            data: image.into(),
            mime_type: "image/png".into(),
        }),
    ];
    snapshot
}

fn images(snapshot: &EvalDetachedCellSnapshot) -> Vec<String> {
    snapshot
        .result
        .content
        .iter()
        .filter_map(|part| match part {
            maho_ai::types::ContentBlock::Image(image) => Some(image.data.clone()),
            _ => None,
        })
        .collect()
}

/// senpi 903b8336 eval-settled-snapshot-budget.test.ts: images spill to disk, come back on get, the
/// oldest files go beyond the disk budget with a note on get, and clear removes the directory.
#[test]
fn spilled_images_hydrate_on_get_and_the_disk_budget_drops_the_oldest() {
    use maho_codemode::tool::settled_image_spill::SettledImageSpill;
    const MIB: usize = 1024 * 1024;
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("settled-images");
    let mut store = TerminalSnapshotStore::with_budget(
        TERMINAL_SNAPSHOT_CAP,
        0,
        Some(SettledImageSpill::new(dir.clone(), 5 * MIB as u64)),
    );
    let image = "A".repeat(2 * MIB);
    for index in 0..4 {
        store.remember(image_snapshot(&format!("fg-{index}"), &image));
    }
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    files.sort();
    assert_eq!(
        files
            .iter()
            .map(|name| name.rsplit_once('-').unwrap().0)
            .collect::<Vec<_>>(),
        ["fg-2", "fg-3"]
    );
    assert!(
        store
            .list()
            .iter()
            .all(|snapshot| images(snapshot).is_empty()),
        "list never reads the disk"
    );
    assert_eq!(
        images(&store.get("fg-3").unwrap()),
        std::slice::from_ref(&image)
    );
    let oldest = store.get("fg-0").unwrap();
    assert!(images(&oldest).is_empty());
    let maho_ai::types::ContentBlock::Text(note) = &oldest.result.content[1] else {
        panic!("lost image note")
    };
    assert!(
        note.text
            .starts_with("[image/png image (2097152 base64 bytes) ")
            && note.text.ends_with(" (ENOENT)]"),
        "{}",
        note.text
    );
    store.clear();
    assert!(!dir.exists());
}
