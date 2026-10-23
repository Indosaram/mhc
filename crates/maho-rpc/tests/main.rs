//! Single integration-test binary for this crate: each file under tests/ is one module, so
//! the crate links one test executable instead of one per file.

#[path = "auth_commands.rs"]
mod auth_commands;
#[path = "close_admission.rs"]
mod close_admission;
#[path = "close_response_reservations.rs"]
mod close_response_reservations;
#[path = "connection_queue.rs"]
mod connection_queue;
#[path = "ensure_endpoint.rs"]
mod ensure_endpoint;
#[path = "extension_dialogs.rs"]
mod extension_dialogs;
#[path = "host_handoff.rs"]
mod host_handoff;
#[path = "host_memory_timer.rs"]
mod host_memory_timer;
#[path = "host_monitors.rs"]
mod host_monitors;
#[path = "host_observed_turns.rs"]
mod host_observed_turns;
#[path = "host_proxy.rs"]
mod host_proxy;
#[path = "host_registration.rs"]
mod host_registration;
#[path = "host_runner.rs"]
mod host_runner;
#[path = "host_stop.rs"]
mod host_stop;
#[path = "host_successor.rs"]
mod host_successor;
#[path = "host_watchdog_binding.rs"]
mod host_watchdog_binding;
#[path = "lag_watchdog_timer.rs"]
mod lag_watchdog_timer;
#[path = "lifecycle_wire.rs"]
mod lifecycle_wire;
#[path = "loaded_surfaces.rs"]
mod loaded_surfaces;
#[path = "loop_lag_shared.rs"]
mod loop_lag_shared;
#[path = "memory_sampler_timer.rs"]
mod memory_sampler_timer;
#[path = "message_commands.rs"]
mod message_commands;
#[path = "native_extension_wire.rs"]
mod native_extension_wire;
#[path = "observer_reactor.rs"]
mod observer_reactor;
#[path = "ownership_lock_bun.rs"]
mod ownership_lock_bun;
#[path = "parser_diagnostics.rs"]
mod parser_diagnostics;
#[path = "print_runner.rs"]
mod print_runner;
#[path = "prompt_hooks.rs"]
mod prompt_hooks;
#[path = "reaper_timer.rs"]
mod reaper_timer;
#[path = "registry_admission.rs"]
mod registry_admission;
#[path = "rpc_event_stream.rs"]
mod rpc_event_stream;
#[path = "rpc_faux_native.rs"]
mod rpc_faux_native;
#[path = "rpc_socket_client.rs"]
mod rpc_socket_client;
#[path = "rpc_socket_close.rs"]
mod rpc_socket_close;
#[path = "session_binding_writer.rs"]
mod session_binding_writer;
#[path = "session_command_persistence.rs"]
mod session_command_persistence;
#[path = "session_entry_wire.rs"]
mod session_entry_wire;
#[path = "session_fanout_actor.rs"]
mod session_fanout_actor;
#[path = "session_idle_timer.rs"]
mod session_idle_timer;
#[path = "session_state_wire.rs"]
mod session_state_wire;
#[path = "session_writer_drain.rs"]
mod session_writer_drain;
#[path = "socket_actor.rs"]
mod socket_actor;
#[path = "socket_identity_wait.rs"]
mod socket_identity_wait;
#[path = "successor_launch.rs"]
mod successor_launch;
#[path = "supersession_timer.rs"]
mod supersession_timer;
#[path = "watchdog_reactor.rs"]
mod watchdog_reactor;
#[path = "worker_controls.rs"]
mod worker_controls;
#[path = "worker_output_credit.rs"]
mod worker_output_credit;
#[path = "worker_protocol.rs"]
mod worker_protocol;
#[path = "worker_requests.rs"]
mod worker_requests;
#[path = "worker_reservation.rs"]
mod worker_reservation;
#[path = "worker_snapshot.rs"]
mod worker_snapshot;
