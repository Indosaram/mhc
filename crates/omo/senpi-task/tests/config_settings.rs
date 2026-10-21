use omo_config_core::schema::resolve_omo_task_settings;
use serde_json::json;

#[test]
fn residency_cap_defaults_to_unlimited_independent_of_cpu_count() {
    for input in [json!({}), json!({"residency_max_children": null})] {
        for cpus in [1, 2, 4, 100] {
            let settings = resolve_omo_task_settings(&input, || cpus).expect("settings");
            assert_eq!(settings["residency_max_children"], json!("unlimited"));
        }
    }
}

#[test]
fn explicit_residency_cap_is_preserved() {
    for cap in [json!(0), json!(3), json!("unlimited")] {
        let input = json!({"residency_max_children": cap, "global_concurrency": 1});
        let settings = resolve_omo_task_settings(&input, || panic!("explicit caps do not need CPU count"))
            .expect("settings");
        assert_eq!(settings["residency_max_children"], cap);
    }
}
