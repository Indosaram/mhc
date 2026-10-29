use super::tests::{commit, files, seed, state};
use super::*;
use crate::sync::item::{self, capture::capture_item, item_frontier, render::render_doc_content};
use loro::ExportMode;

#[test]
fn joined_render_never_vouches_for_whole_value_until_that_value_is_adopted() {
    // Given
    let doc = seed("---\nk: x\n---\nx\n");
    let other = item::create_item_doc(2).expect("peer");
    other
        .import(&doc.export(ExportMode::all_updates()).expect("export"))
        .expect("import");
    let base = render_doc_content(&doc).expect("render");
    let frontier = item_frontier(&doc);
    for (target, value) in [(&doc, "a"), (&other, "b")] {
        let update = capture_item(
            target,
            &base,
            &frontier,
            &format!("---\nk: {value}\n---\nx\n"),
            None,
        )
        .expect("capture")
        .expect("update");
        target.import(&update).expect("import");
    }
    doc.import(&other.export(ExportMode::all_updates()).expect("export"))
        .expect("import");
    let sources = files("---\nk: \"a / b\"\n---\nx\n");
    let plan = plan_adoption(&sources, &state(&doc)).expect("plan");
    let before = confirm_removals(&doc, &plan, &sources).expect("gate");
    assert!(before.remove.is_empty());
    assert_eq!(
        before.retained[0].reason,
        "missing line \"k: \\\"a / b\\\"\""
    );
    // When
    commit(&doc, &plan);
    // Then
    assert_eq!(item::active_registers(&doc)["k"], ["a", "a / b", "b"]);
    assert_eq!(
        confirm_removals(&doc, &plan, &sources)
            .expect("gate")
            .remove,
        [sources[0].path.clone()]
    );
}

#[test]
fn body_gate_refuses_unimported_prediction_even_when_registers_are_already_held() {
    // Given
    let doc = seed("---\nd: space\n---\nx\n");
    let sources = files("---\nd: space\n---\nx\nonly in the side\n");
    let plan = plan_adoption(&sources, &state(&doc)).expect("plan");
    // When
    let decision = confirm_removals(&doc, &plan, &sources).expect("gate");
    // Then
    assert!(decision.remove.is_empty());
    assert_eq!(
        decision.retained,
        [RetainedSource {
            path: sources[0].path.clone(),
            reason: "missing line \"only in the side\"".into()
        }]
    );
}

#[test]
fn case_fold_colliding_main_paths_are_both_adopted() {
    // Given
    let doc = seed("");
    let sources = [
        Snapshot {
            path: "Notes.md".into(),
            text: "abc\nxyz\n".into(),
        },
        Snapshot {
            path: "notes.md".into(),
            text: "ax\n".into(),
        },
    ];
    // When
    let plan = plan_adoption(&sources, &state(&doc)).expect("plan");
    commit(&doc, &plan);
    // Then
    assert_eq!(render_doc_content(&doc).expect("render"), "abc\nxyz\nax\n");
    assert!(plan.removal_candidates.is_empty());
}

#[test]
fn malformed_blocks_never_discard_text_after_frontmatter_fence() {
    // Given
    let header = "<!-- memory-sync 2026-10-15: the other machine had different frontmatter for n.md; kept here so no fact is lost:";
    for inner in ["SECRET FACT\n", "FACT -->\nmore\n"] {
        let text = format!("a\n{header}\n---\nk: v\n---\n{inner}-->\nb\n");
        for path in ["n.md", "n.sync-conflict-20261015-mac.md"] {
            let doc = seed("base\n");
            let sources = [Snapshot {
                path: path.into(),
                text: text.clone(),
            }];
            // When
            let plan = plan_adoption(&sources, &state(&doc)).expect("plan");
            commit(&doc, &plan);
            // Then
            let rendered = render_doc_content(&doc).expect("render");
            for line in inner.lines() {
                assert!(rendered.lines().any(|held| held == line));
            }
        }
    }
}
