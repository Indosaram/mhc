//! senpi 903b8336 test/eval-settled-snapshot-budget.test.ts: settled-cell images spill to disk under
//! the artifacts dir and come back on peek, through the detached-cell manager.

use maho_ai::types::{ContentBlock, ImageContent, TextContent};
use maho_codemode::tool::{
    detached_cell_contract::EvalDetachedCellSnapshot,
    detached_cell_manager::*,
    detached_eval_result::create_detached_control_result,
    terminal_snapshot_store::{TERMINAL_SNAPSHOT_CAP, TerminalSnapshotStore},
    types::{EvalLanguage, EvalToolInput},
};
use maho_ext_api::AgentToolResult;
use serde_json::json;
use std::path::Path;
use std::sync::{Arc, Mutex};

const MIB: usize = 1024 * 1024;

fn two_mib_image() -> String {
    "A".repeat(2 * MIB)
}

fn small_image() -> String {
    "B".repeat(64 * 1024)
}

fn spill_files(root: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(root.join("settled-images")) else {
        return vec![];
    };
    let mut names: Vec<_> = entries
        .map(|entry| {
            entry
                .expect("directory entry")
                .file_name()
                .into_string()
                .expect("valid utf-8 file name")
        })
        .collect();
    names.sort();
    names
}

fn image_result(
    index: usize,
    image: &str,
    json_outputs: Option<serde_json::Value>,
) -> AgentToolResult {
    let mut result = AgentToolResult::text(format!("output-{index}"));
    result.content.push(ContentBlock::Image(ImageContent {
        data: image.into(),
        mime_type: "image/png".into(),
    }));
    result.details = json!({
        "language":"js","languages":["js"],"summary":format!("cell {index}"),"durationMs":1,"toolCalls":[],"truncated":false,
        "cells":[{"index":0,"summary":format!("cell {index}"),"code":index.to_string(),"language":"js","output":format!("output-{index}"),"status":"complete","durationMs":1}]
    });
    if let Some(outputs) = json_outputs {
        result.details["jsonOutputs"] = outputs;
    }
    result
}

fn input(id: &str) -> EvalToolInput {
    EvalToolInput {
        language: EvalLanguage::Js,
        code: id.into(),
        summary: id.into(),
        action: None,
        timeout: None,
        on_timeout: None,
        reset: None,
        isolate: None,
    }
}

fn manager(
    artifacts: Option<&Path>,
    results: Option<u64>,
    images: Option<u64>,
) -> Arc<Mutex<EvalDetachedCellManager>> {
    let defaults = DetachedCellManagerOptions::default();
    Arc::new(Mutex::new(EvalDetachedCellManager::new(
        DetachedCellManagerOptions {
            artifacts_dir: artifacts.map(Path::to_path_buf),
            retained_results_bytes: results.unwrap_or(defaults.retained_results_bytes),
            retained_images_bytes: images.unwrap_or(defaults.retained_images_bytes),
            ..defaults
        },
    )))
}

fn settle_foreground(
    manager: &Arc<Mutex<EvalDetachedCellManager>>,
    id: &str,
    result: AgentToolResult,
) {
    let mut manager = manager.lock().expect("manager lock poisoned");
    let cell = manager.create(id.into(), input(id)).expect("create cell");
    manager.mark_running(&cell);
    assert!(manager.complete(&cell, result));
}

fn settle_detached(
    manager: &Arc<Mutex<EvalDetachedCellManager>>,
    id: &str,
    result: AgentToolResult,
) {
    let mut manager = manager.lock().expect("manager lock poisoned");
    let cell = manager.create(id.into(), input(id)).expect("create cell");
    manager.mark_running(&cell);
    let live = result.clone();
    manager.bind_kernel(
        &cell,
        Arc::new(move || live.clone()),
        Arc::new(|| (None, vec![])),
    );
    assert!(manager.detach(&cell));
    assert!(manager.complete(&cell, result));
}

fn image_data(snapshot: &EvalDetachedCellSnapshot) -> Vec<String> {
    snapshot
        .result
        .content
        .iter()
        .filter_map(|part| match part {
            ContentBlock::Image(image) => Some(image.data.clone()),
            _ => None,
        })
        .collect()
}

fn peek(manager: &Arc<Mutex<EvalDetachedCellManager>>, id: &str) -> EvalDetachedCellSnapshot {
    manager
        .lock()
        .expect("manager lock poisoned")
        .peek(id)
        .expect("cell snapshot peek")
}

#[tokio::test]
async fn a_settled_foreground_cell_returns_its_images_while_memory_holds_none() {
    let root = tempfile::tempdir().unwrap();
    let manager = manager(Some(root.path()), None, None);
    settle_foreground(
        &manager,
        "fg",
        image_result(1, &two_mib_image(), Some(json!([{"answer":42}]))),
    );

    assert_eq!(image_data(&peek(&manager, "fg")), [two_mib_image()]);
    assert_eq!(
        peek(&manager, "fg").result.details["jsonOutputs"],
        json!([{"answer":42}])
    );
    let control = create_detached_control_result(&peek(&manager, "fg"));
    assert_eq!(
        control
            .content
            .iter()
            .filter(|part| matches!(part, ContentBlock::Image(_)))
            .count(),
        1
    );
    let (_, recent) = manager.lock().unwrap().list();
    assert!(
        recent
            .iter()
            .all(|snapshot| image_data(snapshot).is_empty())
    );
    assert_eq!(spill_files(root.path()).len(), 1);
    EvalDetachedCellManager::dispose(&manager).await.unwrap();
}

#[tokio::test]
async fn a_settled_detached_cell_returns_its_images_from_the_spill() {
    let root = tempfile::tempdir().unwrap();
    let manager = manager(Some(root.path()), None, None);
    settle_detached(&manager, "bg", image_result(1, &two_mib_image(), None));

    assert_eq!(image_data(&peek(&manager, "bg")), [two_mib_image()]);
    EvalDetachedCellManager::dispose(&manager).await.unwrap();
}

#[tokio::test]
async fn an_evicted_snapshot_deletes_its_spill_files_and_dispose_removes_the_directory() {
    let root = tempfile::tempdir().unwrap();
    let manager = manager(Some(root.path()), None, None);
    for index in 0..40 {
        settle_foreground(
            &manager,
            &format!("fg-{index}"),
            image_result(index, &small_image(), None),
        );
    }

    assert_eq!(manager.lock().unwrap().list().1.len(), 32);
    assert_eq!(spill_files(root.path()).len(), 32);
    assert!(
        !spill_files(root.path())
            .iter()
            .any(|name| name.starts_with("fg-7-"))
    );
    assert_eq!(image_data(&peek(&manager, "fg-39")), [small_image()]);

    EvalDetachedCellManager::dispose(&manager).await.unwrap();
    assert!(!root.path().join("settled-images").exists());
}

#[tokio::test]
async fn the_disk_budget_deletes_the_oldest_spill_files_and_peek_notes_the_lost_image() {
    let root = tempfile::tempdir().unwrap();
    let manager = manager(Some(root.path()), None, Some(5 * MIB as u64));
    for index in 0..4 {
        settle_foreground(
            &manager,
            &format!("fg-{index}"),
            image_result(index, &two_mib_image(), None),
        );
    }

    let kept: Vec<_> = spill_files(root.path())
        .iter()
        .map(|name| name.rsplit_once('-').unwrap().0.to_owned())
        .collect();
    assert_eq!(kept, ["fg-2", "fg-3"]);
    let oldest = peek(&manager, "fg-0");
    assert!(image_data(&oldest).is_empty());
    let ContentBlock::Text(TextContent { text: first, .. }) = &oldest.result.content[0] else {
        panic!("text output")
    };
    assert_eq!(first, "output-0");
    let ContentBlock::Text(TextContent { text: note, .. }) = &oldest.result.content[1] else {
        panic!("lost image note")
    };
    assert!(
        note.starts_with("[image/png image (2097152 base64 bytes) ")
            && note.ends_with(" (ENOENT)]"),
        "{note}"
    );
    assert_eq!(image_data(&peek(&manager, "fg-3")), [two_mib_image()]);
    EvalDetachedCellManager::dispose(&manager).await.unwrap();
}

#[tokio::test]
async fn peek_answers_with_the_text_and_a_one_line_note_when_a_spill_file_disappeared() {
    let root = tempfile::tempdir().unwrap();
    let manager = manager(Some(root.path()), None, None);
    settle_foreground(&manager, "fg", image_result(1, &small_image(), None));
    for name in spill_files(root.path()) {
        std::fs::remove_file(root.path().join("settled-images").join(name)).unwrap();
    }

    let content = create_detached_control_result(&peek(&manager, "fg")).content;
    assert_eq!(content.len(), 1);
    let ContentBlock::Text(TextContent { text, .. }) = &content[0] else {
        panic!("text")
    };
    assert!(text.contains("output-1\n[image/png image"), "{text}");
    EvalDetachedCellManager::dispose(&manager).await.unwrap();
}

#[tokio::test]
async fn images_stay_in_memory_under_the_byte_budget_without_an_artifacts_dir() {
    let manager = manager(None, Some(5 * MIB as u64), None);
    for index in 0..4 {
        settle_foreground(
            &manager,
            &format!("fg-{index}"),
            image_result(index, &two_mib_image(), None),
        );
    }

    let recent: Vec<_> = manager
        .lock()
        .unwrap()
        .list()
        .1
        .into_iter()
        .map(|snapshot| snapshot.cell_id)
        .collect();
    assert_eq!(recent, ["fg-2", "fg-3"]);
    assert_eq!(image_data(&peek(&manager, "fg-3")), [two_mib_image()]);
    EvalDetachedCellManager::dispose(&manager).await.unwrap();
}

#[test]
fn the_in_memory_budget_holds_except_for_a_single_newest_snapshot_and_zero_keeps_only_the_count_cap()
 {
    let snapshot = |cell_id: &str, image: &str| {
        let mut snapshot = peekless_snapshot(cell_id);
        snapshot.result = image_result(0, image, None);
        snapshot
    };
    let mut budgeted =
        TerminalSnapshotStore::with_budget(TERMINAL_SNAPSHOT_CAP, 3 * MIB as u64, None);
    for index in 0..6 {
        budgeted.remember(snapshot(&format!("s-{index}"), &"A".repeat(MIB)));
        assert!(budgeted.bytes() <= 3 * MIB as u64);
    }
    budgeted.remember(snapshot("huge", &"A".repeat(4 * MIB)));
    assert_eq!(
        budgeted
            .list()
            .iter()
            .map(|entry| entry.cell_id.as_str())
            .collect::<Vec<_>>(),
        ["huge"]
    );

    let mut count_only = TerminalSnapshotStore::with_budget(3, 0, None);
    for index in 0..5 {
        count_only.remember(snapshot(&format!("c-{index}"), &two_mib_image()));
    }
    assert_eq!(
        count_only
            .list()
            .iter()
            .map(|entry| entry.cell_id.as_str())
            .collect::<Vec<_>>(),
        ["c-2", "c-3", "c-4"]
    );
}

fn peekless_snapshot(cell_id: &str) -> EvalDetachedCellSnapshot {
    EvalDetachedCellSnapshot {
        cell_id: cell_id.into(),
        language: EvalLanguage::Js,
        started_at_ms: 0.0,
        state: maho_codemode::tool::detached_cell_contract::EvalDetachedCellState::Completed,
        queued_behind: None,
        output_tail: String::new(),
        result: AgentToolResult::text(""),
        state_retained: Some(true),
        interrupt_note: None,
        hard_limit_seconds: None,
        run_budget_seconds: None,
    }
}
