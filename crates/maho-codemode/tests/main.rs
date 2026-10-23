//! Single integration-test binary for this crate: each file under tests/ is one module, so
//! the crate links one test executable instead of one per file.

#[path = "active_kernel_prelude.rs"]
mod active_kernel_prelude;
#[path = "agent_bridge.rs"]
mod agent_bridge;
#[path = "bridge_http.rs"]
mod bridge_http;
#[path = "bridge_protocol.rs"]
mod bridge_protocol;
#[path = "call_capture.rs"]
mod call_capture;
#[path = "cell_deadlines.rs"]
mod cell_deadlines;
#[path = "cell_execution.rs"]
mod cell_execution;
#[path = "completion_handle_session.rs"]
mod completion_handle_session;
#[path = "completion_handler.rs"]
mod completion_handler;
#[path = "completion_registry.rs"]
mod completion_registry;
#[path = "completion_tool_bridge.rs"]
mod completion_tool_bridge;
#[path = "detached_cell_manager.rs"]
mod detached_cell_manager;
#[path = "detached_cell_notification.rs"]
mod detached_cell_notification;
#[path = "detached_cell_snapshot.rs"]
mod detached_cell_snapshot;
#[path = "detached_cell_state.rs"]
mod detached_cell_state;
#[path = "detached_eval_result.rs"]
mod detached_eval_result;
#[path = "detached_notification_queue.rs"]
mod detached_notification_queue;
#[path = "display_code.rs"]
mod display_code;
#[path = "eval_execution_event.rs"]
mod eval_execution_event;
#[path = "eval_model_context.rs"]
mod eval_model_context;
#[path = "eval_notifier.rs"]
mod eval_notifier;
#[path = "eval_prompt.rs"]
mod eval_prompt;
#[path = "eval_request.rs"]
mod eval_request;
#[path = "eval_schema.rs"]
mod eval_schema;
#[path = "eval_status.rs"]
mod eval_status;
#[path = "eval_status_ticker.rs"]
mod eval_status_ticker;
#[path = "eval_tool_lifecycle.rs"]
mod eval_tool_lifecycle;
#[path = "extension_bindings.rs"]
mod extension_bindings;
#[path = "extension_factory.rs"]
mod extension_factory;
#[path = "host_package_installs.rs"]
mod host_package_installs;
#[path = "image_collector.rs"]
mod image_collector;
#[path = "image_resize.rs"]
mod image_resize;
#[path = "interrupt_bounds.rs"]
mod interrupt_bounds;
#[path = "interrupt_note.rs"]
mod interrupt_note;
#[path = "js_crash_settlement.rs"]
mod js_crash_settlement;
#[path = "js_kernel.rs"]
mod js_kernel;
#[path = "js_kernel_contract.rs"]
mod js_kernel_contract;
#[path = "js_rewrite_imports.rs"]
mod js_rewrite_imports;
#[path = "js_run_queue.rs"]
mod js_run_queue;
#[path = "json_tree.rs"]
mod json_tree;
#[path = "kernel_registry.rs"]
mod kernel_registry;
#[path = "kernel_registry_ownership.rs"]
mod kernel_registry_ownership;
#[path = "kernel_tools_errors.rs"]
mod kernel_tools_errors;
#[path = "kernel_tools_host.rs"]
mod kernel_tools_host;
#[path = "kernel_tools_scope.rs"]
mod kernel_tools_scope;
#[path = "local_module_loader.rs"]
mod local_module_loader;
#[path = "managed_cell.rs"]
mod managed_cell;
#[path = "output_bridge.rs"]
mod output_bridge;
#[path = "output_buffer.rs"]
mod output_buffer;
#[path = "output_meta.rs"]
mod output_meta;
#[path = "process_isolation.rs"]
mod process_isolation;
#[path = "process_tree_host.rs"]
mod process_tree_host;
#[path = "render.rs"]
mod render;
#[path = "render_golden.rs"]
mod render_golden;
#[path = "render_helpers.rs"]
mod render_helpers;
#[path = "reserved_dispatch.rs"]
mod reserved_dispatch;
#[path = "resolve_command.rs"]
mod resolve_command;
#[path = "runtime_info.rs"]
mod runtime_info;
#[path = "runtime_label.rs"]
mod runtime_label;
#[path = "schema_bridge.rs"]
mod schema_bridge;
#[path = "schema_hint.rs"]
mod schema_hint;
#[path = "session_cwd.rs"]
mod session_cwd;
#[path = "session_manager_proxy.rs"]
mod session_manager_proxy;
#[path = "settings.rs"]
mod settings;
#[path = "start_recovery.rs"]
mod start_recovery;
#[path = "status_events.rs"]
mod status_events;
#[path = "streaming_output.rs"]
mod streaming_output;
#[path = "subprocess_queue.rs"]
mod subprocess_queue;
#[path = "terminal_snapshot_store.rs"]
mod terminal_snapshot_store;
#[path = "timeouts.rs"]
mod timeouts;
#[path = "tool_result_marshal.rs"]
mod tool_result_marshal;
#[path = "worker_host.rs"]
mod worker_host;
