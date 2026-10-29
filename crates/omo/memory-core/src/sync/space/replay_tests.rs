use super::tests::{files, state};
use super::*;
use crate::sync::item::{
    self, ItemDevice, capture_on_device,
    capture_receipts::{read_receipts, same_frontier},
    create_device_item_doc, item_frontier,
    render::render_doc_content,
};
use loro::{ExportMode, LoroDoc};

fn fixture(dir: &std::path::Path) -> (ItemDevice, LoroDoc) {
    let device = ItemDevice {
        peer: 1,
        state_dir: dir.into(),
    };
    let doc = create_device_item_doc(&device).expect("device doc");
    let update = capture_on_device(
        &device,
        "n.md",
        &doc,
        "",
        &vec![],
        "---\nd: space\n---\nx\n",
        &[],
    )
    .expect("seed")
    .expect("update");
    doc.import(&update).expect("import");
    (device, doc)
}

#[test]
fn one_update_contains_text_and_extra_values_and_receipts_before_import() {
    // Given: both body and register facts missing from the committed item.
    let dir = tempfile::tempdir().expect("state directory");
    let (device, doc) = fixture(dir.path());
    let sources = files("---\nd: laptop\n---\nx\ny\n");
    let base = render_doc_content(&doc).expect("render");
    let frontier = item_frontier(&doc);
    let plan = plan_adoption(&sources, &state(&doc)).expect("plan");
    let AdoptionAction::Capture {
        working_text,
        extra_register_values,
    } = &plan.action
    else {
        panic!("capture expected");
    };
    // When: the guarded entry point captures both kinds of facts once.
    let update = capture_on_device(
        &device,
        "n.md",
        &doc,
        &base,
        &frontier,
        working_text,
        extra_register_values,
    )
    .expect("capture")
    .expect("update");
    // Then: the receipt precedes import, but prediction alone never authorizes deletion.
    let receipts = read_receipts(&device.state_dir, "n.md").expect("receipts");
    assert!(
        receipts
            .iter()
            .any(|r| same_frontier(&r.base, &frontier) && r.span.is_some())
    );
    assert!(
        confirm_removals(&doc, &plan, &sources)
            .expect("gate")
            .remove
            .is_empty()
    );
    doc.import(&update).expect("import");
    assert_eq!(item::active_registers(&doc)["d"], ["laptop", "space"]);
    assert_eq!(render_doc_content(&doc).expect("render"), plan.rendered);
    assert_eq!(
        confirm_removals(&doc, &plan, &sources)
            .expect("gate")
            .remove,
        [sources[0].path.clone()]
    );
}

#[test]
fn stale_baseline_replay_applies_once_even_after_receipt_only_crash() {
    // Given: the same baseline and source with either a landed or lost first update.
    for lose_first in [false, true] {
        let dir = tempfile::tempdir().expect("state directory");
        let (device, doc) = fixture(dir.path());
        let base = render_doc_content(&doc).expect("render");
        let frontier = item_frontier(&doc);
        let sources = files("---\nd: laptop\n---\nx\ny\n");
        let plan = plan_adoption(&sources, &state(&doc)).expect("plan");
        let AdoptionAction::Capture {
            working_text,
            extra_register_values,
        } = &plan.action
        else {
            panic!("capture expected");
        };
        let first = capture_on_device(
            &device,
            "n.md",
            &doc,
            &base,
            &frontier,
            working_text,
            extra_register_values,
        )
        .expect("capture")
        .expect("update");
        if !lose_first {
            doc.import(&first).expect("import");
        }
        // When: the unchanged stale-baseline request is retried twice.
        for _ in 0..2 {
            if let Some(update) = capture_on_device(
                &device,
                "n.md",
                &doc,
                &base,
                &frontier,
                working_text,
                extra_register_values,
            )
            .expect("replay")
            {
                doc.import(&update).expect("import");
            }
        }
        // Then: no duplicated body lines or register entries.
        assert_eq!(
            render_doc_content(&doc).expect("render"),
            "---\nd: \"laptop / space\"\n---\nx\ny\n"
        );
        assert_eq!(item::active_registers(&doc)["d"], ["laptop", "space"]);
        assert_eq!(
            confirm_removals(&doc, &plan, &sources)
                .expect("gate")
                .remove,
            [sources[0].path.clone()]
        );
    }
}

#[test]
fn superseded_values_are_not_resurrected_and_cannot_authorize_removal() {
    // Given: an adopted register subsequently replaced by another device.
    let dir = tempfile::tempdir().expect("state directory");
    let (device, doc) = fixture(dir.path());
    let base = render_doc_content(&doc).expect("render");
    let frontier = item_frontier(&doc);
    let sources = files("---\nd: laptop\n---\nx\n");
    let plan = plan_adoption(&sources, &state(&doc)).expect("plan");
    let AdoptionAction::Capture {
        working_text,
        extra_register_values,
    } = &plan.action
    else {
        panic!("capture expected");
    };
    assert_eq!(working_text, &base);
    let update = capture_on_device(
        &device,
        "n.md",
        &doc,
        &base,
        &frontier,
        working_text,
        extra_register_values,
    )
    .expect("adopt")
    .expect("register-only update");
    doc.import(&update).expect("import");
    let other_dir = tempfile::tempdir().expect("peer state");
    let peer = ItemDevice {
        peer: 2,
        state_dir: other_dir.path().into(),
    };
    let other = create_device_item_doc(&peer).expect("peer doc");
    other
        .import(&doc.export(ExportMode::all_updates()).expect("export"))
        .expect("import");
    let replacement = capture_on_device(
        &peer,
        "n.md",
        &other,
        &render_doc_content(&other).expect("render"),
        &item_frontier(&other),
        "---\nd: z\n---\nx\n",
        &[],
    )
    .expect("replace")
    .expect("update");
    doc.import(&replacement).expect("import");
    // When: the old adoption request is replayed.
    if let Some(update) = capture_on_device(
        &device,
        "n.md",
        &doc,
        &base,
        &frontier,
        working_text,
        extra_register_values,
    )
    .expect("replay")
    {
        doc.import(&update).expect("import");
    }
    // Then: only the replacement remains active, and the old source must stay.
    assert_eq!(item::active_registers(&doc)["d"], ["z"]);
    let decision = confirm_removals(&doc, &plan, &sources).expect("gate");
    assert!(decision.remove.is_empty());
    assert_eq!(
        decision.retained,
        [RetainedSource {
            path: sources[0].path.clone(),
            reason: "missing line \"d: laptop\"".into(),
        }]
    );
}

#[test]
fn historical_extra_value_over_unchanged_text_writes_nothing_but_receipts_baseline() {
    // Given: a register value in history, superseded by a later capture.
    let dir = tempfile::tempdir().expect("state directory");
    let (device, doc) = fixture(dir.path());
    let old = render_doc_content(&doc).expect("render");
    let update = capture_on_device(
        &device,
        "n.md",
        &doc,
        &old,
        &item_frontier(&doc),
        "---\nd: z\n---\nx\n",
        &[],
    )
    .expect("replace")
    .expect("update");
    doc.import(&update).expect("import");
    let now = render_doc_content(&doc).expect("render");
    let frontier = item_frontier(&doc);
    let before = doc.oplog_vv();
    // When: an already written but inactive value is offered with unchanged text.
    let update = capture_on_device(
        &device,
        "n.md",
        &doc,
        &now,
        &frontier,
        &now,
        &[item::RegisterValue {
            key: "d".into(),
            value: "space".into(),
        }],
    )
    .expect("capture");
    // Then: no new operations, but the baseline has a receipt.
    assert!(update.is_none());
    assert_eq!(doc.oplog_vv(), before);
    assert!(
        read_receipts(&device.state_dir, "n.md")
            .expect("receipts")
            .iter()
            .any(|r| same_frontier(&r.base, &frontier) && r.text == now)
    );
}
