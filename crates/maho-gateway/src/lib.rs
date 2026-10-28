//! Session gateway for the MIT profile (`docs/cleanroom/specs/gateway.md`).
//!
//! A running mhc session can be sent a message by another session (`session_send`) or from the
//! shell (`mhc gateway send`). Long-lived sessions (TUI, RPC, app server) register an endpoint
//! record in the store and poll a per-session inbox directory; each delivery is handed to the
//! session as a displayed `session_control_delivery` custom message — the MIT senpi engine contract
//! already honoured by `maho-core` — delivered as a steer or a follow-up, with a header naming the
//! sender so the model does not take it for the user.
//!
//! See `PROVENANCE.md` for the MIT sources.

pub mod cli;
mod extension;
pub mod message;
pub mod settings;
pub mod store;
pub mod tools;

pub use extension::{GatewayExtension, receives_in};
pub use message::{DELIVERY_SOURCE, DELIVERY_TYPE, Receiver, model_text, to_message};
pub use settings::Settings;
pub use store::{Delivery, Endpoint, Mode, ResolveError, Sender, Store, default_root};
pub use tools::TOOL_NAMES;
