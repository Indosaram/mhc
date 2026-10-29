mod capture;
mod component_list;
mod compose;
mod compose_reload;
mod coordinator;
mod entry;
mod provisioning;
mod reconciliation_9471;
mod scheduler;
mod support;
mod task_coordinator;

#[test]
fn the_registered_loop_component_never_reports_itself_inactive() {
    component_list::the_registered_loop_component_never_reports_itself_inactive();
}
