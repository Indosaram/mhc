use super::*;
use serde_json::json;

fn cycle(stage: CycleStage, progress: Option<Value>, texts: Option<Value>) -> CycleRecord {
    CycleRecord {
        id: "c".into(),
        stage,
        first: 1,
        last: 1,
        batches: vec![],
        keys: vec![],
        tree_id: None,
        progress,
        texts,
    }
}

#[test]
fn progress_marks_landed_by_done_and_texts_are_ignored_at_merged() {
    let c = cycle(
        CycleStage::Merged,
        Some(
            json!({"a": {"text": "x", "done": true}, "b": {"text": null, "done": false}, "c": {"text": 3}}),
        ),
        Some(json!({"z": "ignored"})),
    );
    let renders = legacy_renders(&c);
    assert_eq!(renders.len(), 2);
    assert!(renders["a"].landed);
    assert_eq!(renders["a"].text.as_deref(), Some("x"));
    assert!(!renders["b"].landed);
    assert_eq!(renders["b"].text, None);
}

#[test]
fn texts_count_as_landed_after_the_merged_stage() {
    let c = cycle(
        CycleStage::Rendered,
        None,
        Some(json!({"z": "t", "bad": 1})),
    );
    let renders = legacy_renders(&c);
    assert_eq!(renders.len(), 1);
    assert!(renders["z"].landed);
}
