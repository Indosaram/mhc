mod capture;
mod component_list;
mod compose;
mod compose_reload;
mod coordinator;
mod entry;
mod provisioning;
mod scheduler;
mod support;
mod task_coordinator;

#[test]
fn the_registered_loop_component_logs_through_the_shared_runtime_logger() {
    component_list::the_registered_loop_component_logs_through_the_shared_runtime_logger();
}
