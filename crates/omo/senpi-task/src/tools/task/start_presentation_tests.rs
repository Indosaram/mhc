//! `tools/task/start-presentation.test.ts`

use crate::tools::task::start_presentation::{StartLabels, StartedSummary, background_start_text};

fn started() -> StartedSummary {
    StartedSummary {
        task_id: "st_00000001".to_string(),
        status: "running".to_string(),
        name: "auditor".to_string(),
        queue_position: None,
    }
}

#[test]
fn task_summary_labels_the_task_over_description_and_name() {
    // given / when / then
    let labels = StartLabels {
        task_summary: Some("Audit the boundary".to_string()),
        description: Some("auditing".to_string()),
    };
    assert!(
        background_start_text(&started(), &labels)
            .contains("Started task Audit the boundary (st_00000001, running)")
    );
}

#[test]
fn description_labels_the_task_when_alone() {
    // given / when / then
    let labels = StartLabels {
        task_summary: None,
        description: Some("auditing".to_string()),
    };
    assert!(
        background_start_text(&started(), &labels)
            .contains("Started task auditing (st_00000001, running)")
    );
}

#[test]
fn no_labels_use_the_name_and_keep_the_id_form_stable() {
    // given / when / then
    assert!(
        background_start_text(&started(), &StartLabels::default())
            .contains("Started task auditor (st_00000001, running)")
    );
    let id_named = StartedSummary {
        name: "st_00000001".to_string(),
        ..started()
    };
    assert!(
        background_start_text(&id_named, &StartLabels::default())
            .contains("Started task st_00000001 (running)")
    );
}
