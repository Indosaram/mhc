//! The session gateway: durable delivery between sessions, and the relay that binds a session to a
//! conversation thread.
//!
//! Upstream counterpart: `packages/omo-senpi/src/components/thread/`. One upstream file maps to one
//! module here (`gateway/<name>.ts` -> `thread/<name>.rs`).

pub mod address_book;
pub mod adapter;
pub mod attachment_fence;
pub mod component;
pub mod constants;
pub mod contracts_results;
pub mod decision;
pub mod drain;
pub mod engine;
pub mod errors;
pub mod extension_registrations;
pub mod extension_session_await;
pub mod extension_transaction;
pub mod gateway_replies;
pub mod lazy_live_surface;
pub mod live_surface;
pub mod provenance;
pub mod provenance_config;
pub mod provenance_record;
pub mod provenance_resolver;
pub mod relay;
pub mod result;
pub mod schema;
pub mod sdk;
pub mod session_args_schema;
pub mod store;
pub mod store_api;
pub mod store_extension_facade;
pub mod store_extension_ops;
pub use store_extension_session_ops as extension_session_ops;
pub mod store_extension_session_ops;
pub mod store_extensions;
pub mod store_ops;
pub mod store_relay_ops;
pub mod store_worker;
pub mod tools;
pub mod types;

pub use adapter::{
    Admission, DeliverySender, DrainWakeEvent, EndpointLiveness, GatewayAdmissionKind, GatewayEndpointPort, GatewayEndpointRef, GatewayWakeReply, ReleaseSessionReply,
    SessionAdmissionGate, SessionRuntimePort, WakeReason,
};
pub use constants::{GATEWAY_BUSY_TIMEOUT_MS, GATEWAY_LOCK_WAIT_MAX_MS, GATEWAY_PROVENANCE_SENTENCE, MAX_HOPS};
pub use engine::{GatewayDeliverRequest, GatewayEngine, GatewayEngineOptions, GatewayResolve, GatewayResolution, GatewayTarget};
pub use errors::{ThreadErrorCode, ThreadToolFailure, thread_tool_failure};
pub use provenance::{delivery_provenance_line, delivery_sender, render_delivery_text};
pub use provenance_record::{PROVENANCE_MIGRATION, is_provenance};
pub use provenance_resolver::{ProvenanceRuntime, ProvenanceWorkspace, resolve_provenance};
pub use schema::{GATEWAY_MIGRATIONS, GATEWAY_TABLES, GatewaySchemaVersionError};
pub use store::{GatewayStoreOptions, SqliteGatewayStore, create_gateway_store};
pub use store_api::{GatewayStoreApi, SharedGatewayStore};
pub use store_extensions::{DeclaredSessionOp, SessionCaller, StoreExtensionRegistration, StoreExtensionResult};
pub use types::{DeliveryRow, EnqueueRequest, GatewayDeliveryMode, GatewayDeliveryResult, GatewaySender, Provenance};
