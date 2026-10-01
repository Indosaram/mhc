//! `tools/task/model-visibility.test.ts`

use pretty_assertions::assert_eq;

use crate::state::{ResolvedModelRecord, ResolvedModelSource};
use crate::tools::task::renderers::task_result_lines;
use crate::tools::task::types::{TaskToolDetails, TaskToolItemDetail, TaskToolMode};

/// `RESOLVED_MODEL`: a category-sourced resolution to `quotio-openai/gpt-5.6-luna-fast`.
fn resolved_model() -> ResolvedModelRecord {
    ResolvedModelRecord::new(
        ResolvedModelSource::Category,
        "quotio-openai",
        "gpt-5.6-luna-fast",
    )
}

/// The details `executeBatch` produces for a single background category spawn whose start
/// returned `{ kind: "started", task_id: "st_model", status: "running", name: "model-audit",
/// resolved_model: RESOLVED_MODEL }` for the item `{ category: "quick", name: "model-audit" }`.
fn background_batch_details(resolved: ResolvedModelRecord) -> TaskToolDetails {
    TaskToolDetails {
        task_id: "st_model".to_string(),
        status: "running".to_string(),
        mode: TaskToolMode::Spawn,
        run_in_background: Some(true),
        items: Some(vec![TaskToolItemDetail {
            task_id: "st_model".to_string(),
            name: Some("model-audit".to_string()),
            category: Some("quick".to_string()),
            subagent_type: None,
            model: None,
            resolved_model: Some(resolved),
            status: "running".to_string(),
            queue_position: None,
            ..Default::default()
        }]),
        ..Default::default()
    }
}

#[test]
fn given_a_category_task_resolves_to_a_model_when_batch_spawn_renders_then_the_item_names_both() {
    // given
    let details = background_batch_details(resolved_model());

    // when
    let lines = task_result_lines(&details);
    let item_line = lines.get(1).expect("item line rendered");

    // then
    assert_eq!(lines.len(), 2);
    assert!(
        item_line.contains("category:quick(quotio-openai/gpt-5.6-luna-fast)"),
        "item line was: {item_line}"
    );
    assert!(!item_line.contains("requested/model"), "item line was: {item_line}");
}
