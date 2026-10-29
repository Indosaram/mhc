//! `manager/residency-unlimited.test.ts`.

use std::sync::Arc;
use std::thread;

use serde_json::json;

use super::fakes::{FakeRunner, config, make_lifecycle_manager, named};
use crate::manager::types::{ManagedRunner, StartResult};

#[test]
fn given_unlimited_residency_when_sixteen_children_start_then_all_admitted_and_resident() {
    let runner = FakeRunner::new();
    let harness = make_lifecycle_manager(
        Arc::clone(&runner) as Arc<dyn ManagedRunner>,
        config(16, 1),
        json!({ "residency_max_children": "unlimited" }),
    );

    let results: Vec<StartResult> = thread::scope(|scope| {
        let starts: Vec<_> = (1..=16)
            .map(|index| {
                let manager = &harness.manager;
                scope.spawn(move || manager.start(&named(&format!("child-{index}"))))
            })
            .collect();
        starts
            .into_iter()
            .map(|start| start.join().expect("start thread"))
            .collect()
    });

    assert!(
        results
            .iter()
            .all(|result| matches!(result, StartResult::Started(_))),
        "{results:?}"
    );
    assert_eq!(harness.manager.resident_task_ids().len(), 16);
}
