//! Single integration-test binary for this crate: each file under tests/ is one module, so
//! the crate links one test executable instead of one per file.

#[path = "checkpoint_continuation_test.rs"]
mod checkpoint_continuation_test;
#[path = "checkpoint_core_test.rs"]
mod checkpoint_core_test;
#[path = "checkpoint_final_test.rs"]
mod checkpoint_final_test;
#[path = "checkpoint_status_test.rs"]
mod checkpoint_status_test;
#[path = "checkpoint_template_test.rs"]
mod checkpoint_template_test;
#[path = "checkpoint_test.rs"]
mod checkpoint_test;
#[path = "cli_checkpoint_continuation_test.rs"]
mod cli_checkpoint_continuation_test;
#[path = "cli_checkpoint_test.rs"]
mod cli_checkpoint_test;
#[path = "cli_commands_test.rs"]
mod cli_commands_test;
#[path = "cli_complete_goals_test.rs"]
mod cli_complete_goals_test;
#[path = "cli_create_goals_test.rs"]
mod cli_create_goals_test;
#[path = "cli_entrypoint_test.rs"]
mod cli_entrypoint_test;
#[path = "cli_helpers_test.rs"]
mod cli_helpers_test;
#[path = "cli_json_errors_test.rs"]
mod cli_json_errors_test;
#[path = "cli_scope_required_test.rs"]
mod cli_scope_required_test;
#[path = "cli_status_next_actions_test.rs"]
mod cli_status_next_actions_test;
#[path = "cli_steering_batch_test.rs"]
mod cli_steering_batch_test;
#[path = "cli_steering_kind_guidance_test.rs"]
mod cli_steering_kind_guidance_test;
#[path = "cli_steering_test.rs"]
mod cli_steering_test;
#[path = "cli_test.rs"]
mod cli_test;
#[path = "cli_validation_batch_test.rs"]
mod cli_validation_batch_test;
#[path = "codex_goal_instruction_test.rs"]
mod codex_goal_instruction_test;
#[path = "codex_goal_snapshot_test.rs"]
mod codex_goal_snapshot_test;
#[path = "codex_hook_test.rs"]
mod codex_hook_test;
#[path = "evidence_criteria_gate_test.rs"]
mod evidence_criteria_gate_test;
#[path = "evidence_test.rs"]
mod evidence_test;
#[path = "goal_status_test.rs"]
mod goal_status_test;
#[path = "guided_recovery_test.rs"]
mod guided_recovery_test;
#[path = "package_smoke_test.rs"]
mod package_smoke_test;
#[path = "paths_test.rs"]
mod paths_test;
#[path = "plan_commit_log_test.rs"]
mod plan_commit_log_test;
#[path = "plan_commit_recovery_test.rs"]
mod plan_commit_recovery_test;
#[path = "plan_crud_test.rs"]
mod plan_crud_test;
#[path = "plan_io_cross_process_test.rs"]
mod plan_io_cross_process_test;
#[path = "plan_io_test.rs"]
mod plan_io_test;
#[path = "plan_log_legacy_projection_test.rs"]
mod plan_log_legacy_projection_test;
#[path = "plan_log_newest_test.rs"]
mod plan_log_newest_test;
#[path = "quality_gate_aggregate_basics_test.rs"]
mod quality_gate_aggregate_basics_test;
#[path = "quality_gate_blockers_test.rs"]
mod quality_gate_blockers_test;
#[path = "quality_gate_cap_and_dedupe_test.rs"]
mod quality_gate_cap_and_dedupe_test;
#[path = "quality_gate_doc_test.rs"]
mod quality_gate_doc_test;
#[path = "quality_gate_fields_messages_test.rs"]
mod quality_gate_fields_messages_test;
#[path = "quality_gate_lazycodex_surface_test.rs"]
mod quality_gate_lazycodex_surface_test;
#[path = "quality_gate_poisoning_cascades_test.rs"]
mod quality_gate_poisoning_cascades_test;
#[path = "quality_gate_roles_test.rs"]
mod quality_gate_roles_test;
#[path = "quality_gate_senpi_surface_test.rs"]
mod quality_gate_senpi_surface_test;
#[path = "quality_gate_single_report_test.rs"]
mod quality_gate_single_report_test;
#[path = "quality_gate_test.rs"]
mod quality_gate_test;
#[path = "review_blockers_test.rs"]
mod review_blockers_test;
#[path = "sdk_completed_plan_test.rs"]
mod sdk_completed_plan_test;
#[path = "sdk_contract_test.rs"]
mod sdk_contract_test;
#[path = "sdk_driver_states_test.rs"]
mod sdk_driver_states_test;
#[path = "sdk_dx_test.rs"]
mod sdk_dx_test;
#[path = "sdk_session_context_test.rs"]
mod sdk_session_context_test;
#[path = "sdk_test.rs"]
mod sdk_test;
#[path = "spawn_guard_native_reviewer_test.rs"]
mod spawn_guard_native_reviewer_test;
#[path = "spawn_guard_test.rs"]
mod spawn_guard_test;
#[path = "spawn_role_matrix_test.rs"]
mod spawn_role_matrix_test;
#[path = "spawn_role_registered_test.rs"]
mod spawn_role_registered_test;
#[path = "state_lock_test.rs"]
mod state_lock_test;
#[path = "status_next_actions_surface_test.rs"]
mod status_next_actions_surface_test;
#[path = "steering_batch_test.rs"]
mod steering_batch_test;
#[path = "steering_snapshot_test.rs"]
mod steering_snapshot_test;
#[path = "steering_test.rs"]
mod steering_test;
#[path = "stop_resume_hook_test.rs"]
mod stop_resume_hook_test;
#[path = "surface_test.rs"]
mod surface_test;
#[path = "types_test.rs"]
mod types_test;
#[path = "ultrawork_directive_test.rs"]
mod ultrawork_directive_test;
#[path = "validation_batch_checkpoint_test.rs"]
mod validation_batch_checkpoint_test;
#[path = "validation_batch_test.rs"]
mod validation_batch_test;
