//! Single integration-test binary for this crate: each file under tests/ is one module, so
//! the crate links one test executable instead of one per file.

#[path = "app_server_account.rs"]
mod app_server_account;
#[path = "app_server_approval_bridge.rs"]
mod app_server_approval_bridge;
#[path = "app_server_approval_redaction.rs"]
mod app_server_approval_redaction;
#[path = "app_server_approval_ui_context.rs"]
mod app_server_approval_ui_context;
#[path = "app_server_archive_state.rs"]
mod app_server_archive_state;
#[path = "app_server_catalogs.rs"]
mod app_server_catalogs;
#[path = "app_server_cli_args.rs"]
mod app_server_cli_args;
#[path = "app_server_config.rs"]
mod app_server_config;
#[path = "app_server_connection.rs"]
mod app_server_connection;
#[path = "app_server_core.rs"]
mod app_server_core;
#[path = "app_server_daemon.rs"]
mod app_server_daemon;
#[path = "app_server_daemon_occupancy.rs"]
mod app_server_daemon_occupancy;
#[path = "app_server_daemon_probe.rs"]
mod app_server_daemon_probe;
#[path = "app_server_daemon_process.rs"]
mod app_server_daemon_process;
#[path = "app_server_daemon_state.rs"]
mod app_server_daemon_state;
#[path = "app_server_envelope.rs"]
mod app_server_envelope;
#[path = "app_server_errors.rs"]
mod app_server_errors;
#[path = "app_server_extension.rs"]
mod app_server_extension;
#[path = "app_server_fork.rs"]
mod app_server_fork;
#[path = "app_server_fork_registry.rs"]
mod app_server_fork_registry;
#[path = "app_server_fuzzy_files.rs"]
mod app_server_fuzzy_files;
#[path = "app_server_fuzzy_search.rs"]
mod app_server_fuzzy_search;
#[path = "app_server_generated_protocol.rs"]
mod app_server_generated_protocol;
#[path = "app_server_gitignore.rs"]
mod app_server_gitignore;
#[path = "app_server_goal.rs"]
mod app_server_goal;
#[path = "app_server_goal_event.rs"]
mod app_server_goal_event;
#[path = "app_server_goal_handlers.rs"]
mod app_server_goal_handlers;
#[path = "app_server_history.rs"]
mod app_server_history;
#[path = "app_server_history_pagination.rs"]
mod app_server_history_pagination;
#[path = "app_server_js_semantics.rs"]
mod app_server_js_semantics;
#[path = "app_server_mcp_wire_status.rs"]
mod app_server_mcp_wire_status;
#[path = "app_server_metadata_state.rs"]
mod app_server_metadata_state;
#[path = "app_server_model_list.rs"]
mod app_server_model_list;
#[path = "app_server_models.rs"]
mod app_server_models;
#[path = "app_server_native_faux.rs"]
mod app_server_native_faux;
#[path = "app_server_ndjson.rs"]
mod app_server_ndjson;
#[path = "app_server_notifications.rs"]
mod app_server_notifications;
#[path = "app_server_projection.rs"]
mod app_server_projection;
#[path = "app_server_projection_file_changes.rs"]
mod app_server_projection_file_changes;
#[path = "app_server_projection_message_items.rs"]
mod app_server_projection_message_items;
#[path = "app_server_projection_turn_diff.rs"]
mod app_server_projection_turn_diff;
#[path = "app_server_projection_web_search.rs"]
mod app_server_projection_web_search;
#[path = "app_server_projection_wire_items.rs"]
mod app_server_projection_wire_items;
#[path = "app_server_protocol_facade.rs"]
mod app_server_protocol_facade;
#[path = "app_server_registry.rs"]
mod app_server_registry;
#[path = "app_server_registry_listing.rs"]
mod app_server_registry_listing;
#[path = "app_server_search.rs"]
mod app_server_search;
#[path = "app_server_search_cache.rs"]
mod app_server_search_cache;
#[path = "app_server_search_occurrences.rs"]
mod app_server_search_occurrences;
#[path = "app_server_search_params.rs"]
mod app_server_search_params;
#[path = "app_server_skills.rs"]
mod app_server_skills;
#[path = "app_server_start_options.rs"]
mod app_server_start_options;
#[path = "app_server_stdio.rs"]
mod app_server_stdio;
#[path = "app_server_turn_adapter.rs"]
mod app_server_turn_adapter;
#[path = "app_server_turn_engine.rs"]
mod app_server_turn_engine;
#[path = "app_server_turn_log.rs"]
mod app_server_turn_log;
#[path = "app_server_turn_runtime.rs"]
mod app_server_turn_runtime;
#[path = "app_server_turn_terminal.rs"]
mod app_server_turn_terminal;
#[path = "app_server_unix_socket.rs"]
mod app_server_unix_socket;
#[path = "app_server_user_input_bridge.rs"]
mod app_server_user_input_bridge;
#[path = "app_server_user_input_types.rs"]
mod app_server_user_input_types;
#[path = "app_server_websocket.rs"]
mod app_server_websocket;
#[path = "app_server_websocket_auth.rs"]
mod app_server_websocket_auth;
#[path = "app_server_wire_history.rs"]
mod app_server_wire_history;
#[path = "client.rs"]
mod client;
#[path = "client_service_wire.rs"]
mod client_service_wire;
#[path = "client_unix.rs"]
mod client_unix;
#[path = "protocol_cbor.rs"]
mod protocol_cbor;
#[path = "protocol_codec.rs"]
mod protocol_codec;
#[path = "protocol_framing.rs"]
mod protocol_framing;
#[path = "reconciled_recovery.rs"]
mod reconciled_recovery;
#[path = "server_listener.rs"]
mod server_listener;
#[path = "server_router.rs"]
mod server_router;
#[path = "server_state_codec.rs"]
mod server_state_codec;
#[path = "server_testing.rs"]
mod server_testing;
#[path = "server_unix.rs"]
mod server_unix;
#[path = "sqlite_branch_entries.rs"]
mod sqlite_branch_entries;
#[path = "sqlite_entries.rs"]
mod sqlite_entries;
#[path = "sqlite_repo.rs"]
mod sqlite_repo;
#[path = "sqlite_session_row.rs"]
mod sqlite_session_row;
#[path = "sqlite_sql.rs"]
mod sqlite_sql;
#[path = "sqlite_storage.rs"]
mod sqlite_storage;
#[path = "sqlite_values.rs"]
mod sqlite_values;
