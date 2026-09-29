use omo_config_core::schema::resolve_omo_task_settings;
use serde_json::json;

#[test]
fn residency_cap_defaults_to_max_of_eight_and_cpu_times_three() {
    let parallelism = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    let expected = std::cmp::max(8, parallelism * 3);
    let residency = |cpus: usize| {
        resolve_omo_task_settings(&json!({}), move || cpus).expect("settings")["residency_max_children"].clone()
    };
    assert_eq!(residency(parallelism), json!(expected));
    assert_eq!(residency(2), json!(8));
    assert_eq!(residency(4), json!(12));
}
