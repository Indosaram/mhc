//! `tools/control/send-schema.test.ts`

use pretty_assertions::assert_eq;

use crate::tools::control::send_schema::task_send_params_schema;

fn property_keys() -> Vec<String> {
    let schema = task_send_params_schema();
    schema
        .get("properties")
        .and_then(|properties| properties.as_object())
        .expect("properties object")
        .keys()
        .cloned()
        .collect()
}

#[test]
fn given_the_task_send_schema_when_inspected_then_it_exposes_to_and_hides_old_recipient_keys() {
    let keys = property_keys();

    assert!(keys.iter().any(|key| key == "to"));
    assert!(keys.iter().any(|key| key == "message"));
    assert!(keys.iter().any(|key| key == "team_run_id"));
    assert_eq!(keys.iter().any(|key| key == "task_id"), false);
    assert_eq!(keys.iter().any(|key| key == "name"), false);
}
